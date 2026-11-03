//! Support for quickly finding potential match locations.
use crate::bytesearch::ByteBitmap;
use crate::codepointset;
use crate::insn::StartPredicate;
use crate::ir;
use crate::ir::Node;
use crate::util::{add_utf8_first_bytes_to_bitmap, utf8_first_byte};
#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};
use memchr::memmem;

/// Check if a node is anchored to the start of the line/string.
/// Returns true if the node begins with a StartOfLine anchor.
fn is_start_anchored(n: &Node) -> bool {
    match n {
        Node::Anchor {
            anchor_type: ir::AnchorType::StartOfLine,
            multiline,
        } => !multiline,
        Node::Cat(nodes) => {
            // For concatenation, check if the first node is start-anchored
            nodes.first().is_some_and(is_start_anchored)
        }
        Node::CaptureGroup { contents, .. } => is_start_anchored(contents),
        // Other nodes are not anchored
        _ => false,
    }
}

/// Convert the code point set to a first-byte bitmap.
/// That is, make a list of all of the possible first bytes of every contained
/// code point, and store that in a bitmap.
fn cps_to_first_byte_bitmap(input: &codepointset::CodePointSet) -> Box<ByteBitmap> {
    let mut bitmap = Box::<ByteBitmap>::default();
    for iv in input.intervals() {
        add_utf8_first_bytes_to_bitmap(*iv, &mut bitmap);
    }
    bitmap
}

/// The "IR" for a start predicate.
enum AbstractStartPredicate {
    /// No predicate.
    Arbitrary,

    /// Sequence of non-empty bytes.
    Sequence(Vec<u8>),

    /// Set of bytes.
    Set(Box<ByteBitmap>),
}

impl AbstractStartPredicate {
    /// \return the disjunction of two predicates.
    /// That is, a predicate that matches x OR y.
    fn disjunction(x: Self, y: Self) -> Self {
        match (x, y) {
            (Self::Arbitrary, _) => Self::Arbitrary,
            (_, Self::Arbitrary) => Self::Arbitrary,

            (Self::Sequence(s1), Self::Sequence(s2)) => {
                // Compute the length of the shared prefix.
                let shared_len = s1.iter().zip(s2.iter()).take_while(|(a, b)| a == b).count();
                debug_assert!(s1[..shared_len] == s2[..shared_len]);
                if shared_len > 0 {
                    // Use the shared prefix.
                    Self::Sequence(s1[..shared_len].to_vec())
                } else {
                    // Use a set of their first byte.
                    Self::Set(Box::new(ByteBitmap::new(&[s1[0], s2[0]])))
                }
            }

            (Self::Set(mut s1), Self::Set(s2)) => {
                s1.bitor(s2.as_ref());
                Self::Set(s1)
            }

            (Self::Set(mut s1), Self::Sequence(s2)) => {
                // Add first byte to set.
                s1.set(s2[0]);
                Self::Set(s1)
            }

            (Self::Sequence(s1), Self::Set(mut s2)) => {
                s2.set(s1[0]);
                Self::Set(s2)
            }
        }
    }

    /// Resolve ourselves to a concrete start predicate.
    fn resolve_to_insn(self) -> StartPredicate {
        match self {
            Self::Arbitrary => StartPredicate::Arbitrary,
            Self::Sequence(vals) => match vals.len() {
                0 => StartPredicate::Arbitrary,
                1 => StartPredicate::ByteSet1([vals[0]]),
                _ => StartPredicate::ByteSeq(Box::new(memmem::Finder::new(&vals).into_owned())),
            },
            Self::Set(bm) => match bm.count_bits() {
                0 => StartPredicate::Arbitrary,
                1 => StartPredicate::ByteSet1(bm.as_array()),
                2 => StartPredicate::ByteSet2(bm.as_array()),
                3 => StartPredicate::ByteSet3(bm.as_array()),
                _ => StartPredicate::ByteBracket(*bm),
            },
        }
    }
}

/// Compute any start-predicate for a node..
/// If this returns None, then the instruction is conceptually zero-width (e.g.
/// lookahead assertion) and does not contribute to the predicate.
/// If this returns StartPredicate::Arbitrary, then there is no predicate.
fn compute_start_predicate(n: &Node) -> Option<AbstractStartPredicate> {
    let arbitrary = Some(AbstractStartPredicate::Arbitrary);
    match n {
        Node::ByteSequence(bytevec) => Some(AbstractStartPredicate::Sequence(bytevec.clone())),
        Node::ByteSet(bytes) => Some(AbstractStartPredicate::Set(Box::new(ByteBitmap::new(
            bytes,
        )))),

        Node::Empty => arbitrary,
        Node::Goal => arbitrary,
        Node::BackRef { .. } | Node::NamedBackRef { .. } => arbitrary,

        Node::CharSet(chars) => {
            // Pick the first bytes out.
            let bytes = chars
                .iter()
                .map(|&c| utf8_first_byte(c))
                .collect::<Vec<_>>();
            Some(AbstractStartPredicate::Set(Box::new(ByteBitmap::new(
                &bytes,
            ))))
        }

        // We assume that most char nodes have been optimized to ByteSeq or AnyBytes2, so skip
        // these.
        // TODO: we could support icase through bitmap of de-folded first bytes.
        Node::Char { .. } => arbitrary,

        // Cats return the first non-None value, if any.
        Node::Cat(nodes) => nodes.iter().filter_map(compute_start_predicate).next(),

        // MatchAny (aka .) is too common to do a fast prefix search for.
        Node::MatchAny => arbitrary,

        // MatchAnyExceptLineTerminator (aka .) is too common to do a fast prefix search for.
        Node::MatchAnyExceptLineTerminator => arbitrary,

        // TODO: can probably exploit some of these.
        Node::Anchor { .. } => arbitrary,
        Node::WordBoundary { .. } => arbitrary,

        // Capture groups delegate to their contents.
        Node::CaptureGroup { contents, .. } => compute_start_predicate(contents),

        // Zero-width assertions are one of the few instructions that impose no start predicate.
        Node::LookaroundAssertion { .. } => None,

        Node::Loop { loopee, quant, .. } => {
            // TODO: we could try to join two predicates if the loop were optional.
            if quant.min > 0 {
                compute_start_predicate(loopee)
            } else {
                arbitrary
            }
        }

        Node::Loop1CharBody { loopee, quant } => {
            // TODO: we could try to join two predicates if the loop were optional.
            if quant.min > 0 {
                compute_start_predicate(loopee)
            } else {
                arbitrary
            }
        }

        // This one is interesting - we compute the disjunction of the predicates of our two arms.
        Node::Alt(left, right) => {
            if let (Some(x), Some(y)) = (
                compute_start_predicate(left),
                compute_start_predicate(right),
            ) {
                Some(AbstractStartPredicate::disjunction(x, y))
            } else {
                // This indicates that one of our branches could match the empty string.
                arbitrary
            }
        }

        // Brackets get a bitmap.
        Node::Bracket(bc) => {
            // If our bracket is inverted, construct the set of code points not contained.
            let storage;
            let cps = if bc.invert {
                storage = bc.cps.inverted();
                &storage
            } else {
                &bc.cps
            };
            let bitmap = cps_to_first_byte_bitmap(cps);
            Some(AbstractStartPredicate::Set(bitmap))
        }
    }
}

/// PATCH (perf, see VENDORED.md): the start search for UTF-16 and UCS-2
/// input. The byte predicates above only serve byte-element inputs, so a
/// non-ASCII subject ran a full match attempt at every position: `/\d+/g`
/// over 70 units of accented text made 70 attempts to find two digits. This
/// filter answers "can a match start with this code unit": a bit per unit
/// below 256, and above that a bit per high byte (a whole 256-unit page),
/// which is exact for the ASCII and Latin-1 punctuation that separates
/// words and still rules out whole scripts. A code point above the BMP
/// starts with its high surrogate, so it marks that surrogate's page. The
/// filter is a necessary condition only; every candidate still gets a full
/// attempt.
#[cfg(feature = "utf16")]
#[derive(Debug, Clone)]
pub struct UnitFilter {
    lo: [u64; 4],
    hi: [u64; 4],
}

#[cfg(feature = "utf16")]
impl UnitFilter {
    #[inline(always)]
    pub fn contains(&self, u: u16) -> bool {
        let (map, b) = if u < 256 {
            (&self.lo, u as usize)
        } else {
            (&self.hi, (u >> 8) as usize)
        };
        (map[b >> 6] >> (b & 63)) & 1 != 0
    }

    /// Units `a..=b` (both at most 0xFFFF).
    fn add_units(&mut self, a: u32, b: u32) {
        let mut u = a;
        while u <= b && u < 256 {
            self.lo[(u >> 6) as usize] |= 1 << (u & 63);
            u += 1;
        }
        if b >= 256 {
            for page in (a.max(256) >> 8)..=(b >> 8) {
                self.hi[(page >> 6) as usize] |= 1 << (page & 63);
            }
        }
    }

    fn from_cps(cps: &codepointset::CodePointSet) -> Self {
        let mut f = UnitFilter {
            lo: [0; 4],
            hi: [0; 4],
        };
        for iv in cps.intervals() {
            let (a, b) = (iv.first, iv.last);
            if a <= 0xFFFF {
                f.add_units(a, b.min(0xFFFF));
            }
            if b >= 0x1_0000 {
                let a = a.max(0x1_0000);
                f.add_units(0xD800 + ((a - 0x1_0000) >> 10), 0xD800 + ((b - 0x1_0000) >> 10));
            }
        }
        f
    }
}

/// What a node says about the first element of a match it takes part in.
#[cfg(feature = "utf16")]
enum First {
    /// Consumes nothing (an assertion, the empty string).
    Zero,
    /// No constraint.
    Any,
    /// Consumes an element from the set, or nothing at all.
    Opt(codepointset::CodePointSet),
    /// Consumes an element, always one from the set.
    Set(codepointset::CodePointSet),
}

#[cfg(feature = "utf16")]
fn first_elements(n: &Node) -> First {
    use codepointset::{CodePointSet, Interval};
    let one = |c: u32| {
        let mut s = CodePointSet::new();
        s.add(Interval::new(c, c));
        s
    };
    match n {
        Node::Empty | Node::Anchor { .. } | Node::WordBoundary { .. } => First::Zero,
        // A case-folded char matches every char that folds to it, which this
        // does not enumerate; a backreference may be empty; reaching the goal
        // ends the match where it stands.
        Node::Goal | Node::BackRef { .. } | Node::NamedBackRef { .. } => First::Any,
        Node::Char { icase: true, .. } => First::Any,
        &Node::Char { c, icase: false } => First::Set(one(c)),
        Node::CharSet(chars) => {
            let mut s = CodePointSet::new();
            for &c in chars {
                s.add(Interval::new(c, c));
            }
            First::Set(s)
        }
        // Byte nodes belong to byte programs; only an ASCII byte names its
        // character.
        Node::ByteSequence(bytes) => match bytes.first() {
            Some(&b) if b < 0x80 => First::Set(one(u32::from(b))),
            _ => First::Any,
        },
        Node::ByteSet(bytes) if bytes.iter().all(|&b| b < 0x80) => {
            let mut s = CodePointSet::new();
            for &b in bytes {
                s.add(Interval::new(u32::from(b), u32::from(b)));
            }
            First::Set(s)
        }
        Node::ByteSet(_) => First::Any,
        Node::MatchAny | Node::MatchAnyExceptLineTerminator => First::Any,
        Node::Bracket(bc) => First::Set(if bc.invert {
            bc.cps.inverted()
        } else {
            bc.cps.clone()
        }),
        Node::Cat(nodes) => {
            let mut acc: Option<CodePointSet> = None;
            for node in nodes {
                match first_elements(node) {
                    First::Zero => {}
                    First::Any => return First::Any,
                    First::Opt(s) => match &mut acc {
                        Some(a) => a.add_set(s),
                        None => acc = Some(s),
                    },
                    First::Set(s) => {
                        return First::Set(match acc {
                            Some(mut a) => {
                                a.add_set(s);
                                a
                            }
                            None => s,
                        });
                    }
                }
            }
            match acc {
                Some(a) => First::Opt(a),
                None => First::Zero,
            }
        }
        Node::Alt(left, right) => match (first_elements(left), first_elements(right)) {
            (First::Any, _) | (_, First::Any) => First::Any,
            (First::Zero, First::Zero) => First::Zero,
            (First::Zero, First::Set(s) | First::Opt(s))
            | (First::Set(s) | First::Opt(s), First::Zero) => First::Opt(s),
            (First::Set(mut a), First::Set(b)) => {
                a.add_set(b);
                First::Set(a)
            }
            (First::Set(mut a) | First::Opt(mut a), First::Set(b) | First::Opt(b)) => {
                a.add_set(b);
                First::Opt(a)
            }
        },
        Node::CaptureGroup { contents, .. } => first_elements(contents),
        // A positive lookahead's first element is the one at this position;
        // a lookbehind or a negated assertion says nothing about it.
        Node::LookaroundAssertion {
            negate: false,
            backwards: false,
            contents,
            ..
        } => match first_elements(contents) {
            First::Set(s) => First::Set(s),
            _ => First::Zero,
        },
        Node::LookaroundAssertion { .. } => First::Zero,
        Node::Loop { loopee, quant, .. } | Node::Loop1CharBody { loopee, quant } => {
            match first_elements(loopee) {
                First::Set(s) if quant.min == 0 => First::Opt(s),
                other => other,
            }
        }
    }
}

/// The UTF-16 / UCS-2 start filter for a regex, when every match must begin
/// with an element the filter can rule on (see [`UnitFilter`]).
#[cfg(feature = "utf16")]
pub fn unit_filter_for_re(re: &ir::Regex) -> Option<Box<UnitFilter>> {
    match first_elements(&re.node) {
        First::Set(s) => {
            let f = UnitFilter::from_cps(&s);
            // Every unit passes: the filter would only cost its lookups.
            let all = f.lo.iter().all(|&w| w == u64::MAX)
                && f.hi.iter().enumerate().all(|(i, &w)| w | u64::from(i == 0) == u64::MAX);
            (!all).then(|| Box::new(f))
        }
        _ => None,
    }
}

/// \return the start predicate for a Regex.
pub fn predicate_for_re(re: &ir::Regex) -> StartPredicate {
    // Check if the regex is anchored to the start - if so, we can optimize
    // by avoiding string searching entirely. However, only do this when
    // multiline mode is disabled, since in multiline mode ^ can match
    // at the beginning of any line, not just the string start.
    if is_start_anchored(&re.node) && !re.flags.multiline {
        return StartPredicate::StartAnchored;
    }

    compute_start_predicate(&re.node)
        .unwrap_or(AbstractStartPredicate::Arbitrary)
        .resolve_to_insn()
}
