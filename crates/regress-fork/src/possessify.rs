//! Auto-possessification of single-char greedy loops (PATCH, see VENDORED.md).
//!
//! Phase 1 (the `possessive` flag on `Insn::Loop1CharBody`): a greedy
//! one-char loop whose character class is provably disjoint from the first
//! character set of the instruction that follows it never benefits from
//! backtracking, so the executor can skip pushing the `GreedyLoop1Char`
//! backtrack entry entirely.
//!
//! Soundness (Phase 1): suppose the loop matched k iterations from position
//! p0, ending at position q (the run p0..q consists solely of loop-class
//! characters), and the continuation later fails, causing a backtrack to
//! j < k iterations. That leaves the cursor at some position r with
//! p0 <= r < q, and the character at r is a loop-class character (it was
//! consumed as iteration j+1 of the original run). The continuation begins —
//! after zero-width, unconditional capture-group markers and jumps, which we
//! skip below — with a consuming instruction f whose first-set is disjoint
//! from the loop class, so f fails immediately at r. Hence every backtrack
//! retry fails and the backtrack entry is dead. Note this argument only
//! visits positions strictly inside the matched run, so it does NOT depend on
//! why the run ended (class mismatch, end of input, or a bounded quantifier
//! hitting max): possessification is sound for bounded loops like `\d{1,3}`
//! too. If the follow is `Goal` the match has already succeeded and nothing
//! after it can fail, so the entry is equally dead.
//!
//! Phase 2 (`skip_hint_ip` on `CompiledRegex`): when the pattern's FIRST
//! consuming atom (allowing one peeled single-char atom before it — the
//! optimizer rewrites `c+` as `c c*`; see the argument at the Phase 2 scan
//! below) is a possessive, greedy, UNBOUNDED one-char loop, a failed
//! match attempt at start p proves there is no match starting anywhere inside
//! the maximal run either, so the search may resume at the run's end instead
//! of p+1.
//!
//! Soundness (Phase 2): let the maximal run from p end at position e (the
//! character at e, if any, is not a loop-class character — for an unbounded
//! loop the run always ends on class mismatch or end of input). Consider any
//! candidate start q with p < q < e. Every character in q..e is loop-class
//! (a suffix of the run from p). Because the loop is unbounded, the maximal
//! run from q ends at the same e. Two cases for a hypothetical match at q:
//! either the loop cannot reach min iterations (fewer than min loop-class
//! characters remain before e), and the attempt fails outright — this covers
//! every min, including min > 1; or it can, and then the greedy loop first
//! tries the maximal run ending at e, where the rest of the pattern fails
//! exactly as it did during the attempt at p: the whole pattern is rejected
//! by this pass if it contains lookaround or backreferences, so whether the
//! rest of the pattern matches from a given position depends only on that
//! position, not on how we got there (capture-group state can only be
//! observed through backreferences, and loops entered in the rest are reset
//! by `EnterLoop`). Every shorter iteration count leaves the continuation at
//! a position in q..e whose character is loop-class, where the disjoint
//! first-set instruction f fails immediately (the Phase 1 argument). So no
//! match starts in (p, e), and resuming the search at e is exact.
//!
//! This restriction deliberately excludes bounded quantifiers: for
//! `/(\d{1,3})\./` on `"12345.6"` the attempt at 0 consumes "123" (max hit)
//! and fails at '4', but a real match ("345.") starts at offset 2 — a
//! bounded loop's maximal run from a later start can end LATER than the run
//! from p, so the "same failing position" argument collapses. Phase 1 still
//! applies to such loops (see above); only the skip hint is withheld.
//!
//! `ZIPP_NO_RX_POSSESS=1` disables both phases; it is read once and gates the
//! pass at compile (emit) time, so patterns compiled while it is set carry
//! neither possessive marks nor a skip hint.

use crate::codepointset::CodePointSet;
use crate::insn::{CompiledRegex, Insn};

/// Whether the pass is enabled (cached read of `ZIPP_NO_RX_POSSESS`).
pub(crate) fn enabled() -> bool {
    #[cfg(feature = "std")]
    {
        use core::sync::atomic::{AtomicU8, Ordering};
        // 0 = unread; 1 = disabled; 2 = enabled.
        static CACHE: AtomicU8 = AtomicU8::new(0);
        match CACHE.load(Ordering::Relaxed) {
            0 => {
                let v = std::env::var_os("ZIPP_NO_RX_POSSESS").is_none() as u8;
                CACHE.store(v + 1, Ordering::Relaxed);
                v == 1
            }
            v => v == 2,
        }
    }
    #[cfg(not(feature = "std"))]
    {
        true
    }
}

/// \return the exact set of code points instruction `insn` can consume as its
/// first character, or None if we cannot (or choose not to) compute it.
/// Byte-oriented instructions are only trusted below 0x80, where a code unit
/// equals its code point in every supported encoding.
fn first_set(re: &CompiledRegex, insn: &Insn) -> Option<CodePointSet> {
    let mut set = CodePointSet::new();
    match insn {
        &Insn::Char(c) => set.add_one(c),
        &Insn::Bracket(idx) => {
            let bc = &re.brackets[idx];
            if bc.invert {
                set = bc.cps.inverted();
            } else {
                set = bc.cps.clone();
            }
        }
        Insn::AsciiBracket(bitmap) => {
            use crate::bytesearch::ByteSet;
            for b in 0..128u8 {
                if bitmap.contains(b) {
                    set.add_one(b as u32);
                }
            }
        }
        Insn::CharSet(chars) => {
            for &c in chars.iter() {
                set.add_one(c);
            }
        }
        &Insn::ByteSet2(bytes) => add_ascii_bytes(&mut set, &bytes.0)?,
        &Insn::ByteSet3(bytes) => add_ascii_bytes(&mut set, &bytes.0)?,
        &Insn::ByteSet4(bytes) => add_ascii_bytes(&mut set, &bytes.0)?,
        // Only the first byte matters: if it cannot match, the sequence fails.
        Insn::ByteSeq1(b) => add_ascii_bytes(&mut set, &b[..1])?,
        Insn::ByteSeq2(b) => add_ascii_bytes(&mut set, &b[..1])?,
        Insn::ByteSeq3(b) => add_ascii_bytes(&mut set, &b[..1])?,
        Insn::ByteSeq4(b) => add_ascii_bytes(&mut set, &b[..1])?,
        Insn::ByteSeq5(b) => add_ascii_bytes(&mut set, &b[..1])?,
        Insn::ByteSeq6(b) => add_ascii_bytes(&mut set, &b[..1])?,
        Insn::ByteSeq7(b) => add_ascii_bytes(&mut set, &b[..1])?,
        Insn::ByteSeq8(b) => add_ascii_bytes(&mut set, &b[..1])?,
        Insn::ByteSeq9(b) => add_ascii_bytes(&mut set, &b[..1])?,
        Insn::ByteSeq10(b) => add_ascii_bytes(&mut set, &b[..1])?,
        Insn::ByteSeq11(b) => add_ascii_bytes(&mut set, &b[..1])?,
        Insn::ByteSeq12(b) => add_ascii_bytes(&mut set, &b[..1])?,
        Insn::ByteSeq13(b) => add_ascii_bytes(&mut set, &b[..1])?,
        Insn::ByteSeq14(b) => add_ascii_bytes(&mut set, &b[..1])?,
        Insn::ByteSeq15(b) => add_ascii_bytes(&mut set, &b[..1])?,
        Insn::ByteSeq16(b) => add_ascii_bytes(&mut set, &b[..1])?,
        // Everything else (CharICase, MatchAny, anchors, word boundaries,
        // Alt, loops, ...) is either not a plain consuming instruction or not
        // worth modelling: be conservative.
        _ => return None,
    }
    Some(set)
}

/// Add each byte as a code point; fail if any is non-ASCII.
fn add_ascii_bytes(set: &mut CodePointSet, bytes: &[u8]) -> Option<()> {
    for &b in bytes {
        if b >= 0x80 {
            return None;
        }
        set.add_one(b as u32);
    }
    Some(())
}

/// \return whether two code point sets share no code point.
fn disjoint(a: &CodePointSet, b: &CodePointSet) -> bool {
    let (av, bv) = (a.intervals(), b.intervals());
    let (mut i, mut j) = (0, 0);
    while i < av.len() && j < bv.len() {
        if av[i].overlaps(bv[j]) {
            return false;
        }
        if av[i].last < bv[j].last {
            i += 1;
        } else {
            j += 1;
        }
    }
    true
}

/// Skipping zero-width unconditional instructions (capture-group markers and
/// jumps), find the instruction that consumes the continuation's first
/// character starting at index `j`. \return its index, or None if we hit
/// anything we do not model.
fn follow_insn(re: &CompiledRegex, mut j: usize) -> Option<usize> {
    // Budget guards against jump cycles (which well-formed programs lack).
    let mut budget = 64usize;
    while j < re.insns.len() && budget > 0 {
        budget -= 1;
        match &re.insns[j] {
            Insn::BeginCaptureGroup(_) | Insn::EndCaptureGroup(_) | Insn::ResetCaptureGroup(_) => {
                j += 1
            }
            &Insn::Jump { target } => j = target as usize,
            _ => return Some(j),
        }
    }
    None
}

/// Run the pass over a freshly emitted regex, marking eligible loops
/// possessive and (when safe) recording the Phase 2 skip hint.
pub(crate) fn apply(re: &mut CompiledRegex) {
    // Exclude any pattern with lookaround or backreferences: the Phase 2
    // argument requires "rest of pattern matches from a position" to be a
    // function of the position alone, and lookbehinds additionally execute
    // instructions right-to-left, which would invert the follow relation.
    for insn in &re.insns {
        match insn {
            Insn::Lookahead { .. }
            | Insn::Lookbehind { .. }
            | Insn::BackRef { .. }
            | Insn::BackRefMulti { .. } => return,
            _ => {}
        }
    }

    // Phase 1: mark possessive loops.
    for i in 0..re.insns.len() {
        let (greedy, has_body) = match re.insns[i] {
            Insn::Loop1CharBody { greedy, .. } => (greedy, i + 1 < re.insns.len()),
            _ => continue,
        };
        if !greedy || !has_body {
            continue;
        }
        let Some(body_set) = first_set(re, &re.insns[i + 1]) else {
            continue;
        };
        let Some(f) = follow_insn(re, i + 2) else {
            continue;
        };
        let dead_backtrack = match &re.insns[f] {
            // Nothing after a Goal can fail; the backtrack entry is dead.
            Insn::Goal => true,
            follow => match first_set(re, follow) {
                Some(follow_set) => disjoint(&body_set, &follow_set),
                None => false,
            },
        };
        if dead_backtrack {
            match &mut re.insns[i] {
                Insn::Loop1CharBody { possessive, .. } => *possessive = true,
                _ => unreachable!("Checked above"),
            }
        }
    }

    // Phase 2: the failed-run skip hint. Only for the pattern's first
    // consuming atom (nothing before it but capture-group entries and at most
    // ONE peeled single-char atom — the optimizer rewrites `c+` as `c c*`),
    // and only for UNBOUNDED possessive greedy loops — see the module comment
    // for why bounded quantifiers are excluded. A single peeled atom C is
    // safe: for any candidate start q inside the failed run (whose chars are
    // all loop-class), either C fails at q and the attempt dies, or C
    // consumes exactly the one element at q — it can never cross the run end
    // e — and the loop then reaches the same maximal end e, where the rest of
    // the pattern fails exactly as before. Two or more peeled atoms could
    // consume past e from q = e-1, which would break the argument, so only
    // one is allowed.
    let mut k = 0usize;
    let mut peeled = false;
    while k < re.insns.len() {
        match &re.insns[k] {
            Insn::BeginCaptureGroup(_) | Insn::ResetCaptureGroup(_) => k += 1,
            &Insn::Loop1CharBody {
                max_iters,
                greedy,
                possessive,
                ..
            } => {
                if greedy && possessive && max_iters == usize::MAX {
                    re.skip_hint_ip = Some(k as u32);
                }
                break;
            }
            insn if !peeled && consumes_exactly_one_element(insn) => {
                peeled = true;
                k += 1;
            }
            _ => break,
        }
    }
}

/// \return whether `insn` always consumes exactly one input element when it
/// matches (the Phase 2 peel must never be able to consume past the run end).
fn consumes_exactly_one_element(insn: &Insn) -> bool {
    matches!(
        insn,
        Insn::Char(_)
            | Insn::CharICase(_)
            | Insn::Bracket(_)
            | Insn::AsciiBracket(_)
            | Insn::CharSet(_)
            | Insn::MatchAny
            | Insn::MatchAnyExceptLineTerminator
            | Insn::ByteSet2(_)
            | Insn::ByteSet3(_)
            | Insn::ByteSet4(_)
            | Insn::ByteSeq1(_)
    )
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use crate::insn::Insn;

    fn compiled(pattern: &str) -> crate::insn::CompiledRegex {
        let flags = crate::api::Flags::default();
        let mut ire = crate::parse::try_parse(pattern.chars().map(u32::from), flags).unwrap();
        crate::optimizer::optimize(&mut ire);
        crate::emit::emit(&ire)
    }

    fn possessive_loops(re: &crate::insn::CompiledRegex) -> Vec<bool> {
        re.insns
            .iter()
            .filter_map(|i| match i {
                &Insn::Loop1CharBody { possessive, .. } => Some(possessive),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn marks_disjoint_loops_and_first_atom_hint() {
        let re = compiled(r"([a-z]+)=(\d+)");
        assert_eq!(possessive_loops(&re), [true, true]);
        assert!(
            re.skip_hint_ip.is_some(),
            "unbounded first atom gets the hint"
        );
    }

    #[test]
    fn bounded_first_atom_gets_no_hint() {
        let re = compiled(r"(\d{1,3})\.");
        assert_eq!(possessive_loops(&re), [true]);
        assert_eq!(
            re.skip_hint_ip, None,
            "bounded loops must never get the skip"
        );
    }

    #[test]
    fn overlapping_follow_not_possessive() {
        let re = compiled(r"([a-z]+)e");
        assert_eq!(possessive_loops(&re), [false]);
        assert_eq!(re.skip_hint_ip, None);
    }

    #[test]
    fn lookaround_and_backref_excluded() {
        assert_eq!(possessive_loops(&compiled(r"([a-z]+)(?==)")), [false]);
        assert_eq!(possessive_loops(&compiled(r"([a-z]+)\1=")), [false]);
    }
}
