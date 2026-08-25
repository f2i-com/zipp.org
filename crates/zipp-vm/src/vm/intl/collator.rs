#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;
use crate::vm::*;
use crate::vm::{cldr_en, dtf_pattern};

impl<'p> Vm<'p> {
    /// CompareStrings for a resolved `Intl.Collator`.
    ///
    /// **There is still no DUCET/CLDR collation here.** What this DOES implement
    /// is the part of ECMA-402 §10.3.2 that is not weight data at all: the
    /// multi-level comparison the four `sensitivity` values name. A string is
    /// decomposed (NFD) into three keys —
    ///
    /// * **primary** — the base characters (combining marks removed), lowercased
    /// * **secondary** — the combining marks, in order
    /// * **tertiary** — the case of each base character
    ///
    /// — and `sensitivity` chooses how many of them count: `"base"` primary
    /// only, `"accent"` primary+secondary, `"case"` primary+tertiary, `"variant"`
    /// all three. Ties at the tertiary level order lowercase before uppercase,
    /// which is the DUCET tertiary weight ordering (0x0002 for small letters,
    /// 0x0008 for capitals) and what `caseFirst: "upper"` exists to reverse.
    ///
    /// The primary level still orders base characters **by code point**, because
    /// zipp ships no `allkeys_CLDR.txt`. That agrees with the root collation
    /// across ASCII Latin and any script whose code points already run in
    /// alphabetical order, and is not guaranteed to elsewhere — a real weight
    /// table is the only fix for that, and
    /// `Collator/prototype/compare/non-normative-*.js` are the tests that would
    /// notice. Locale collation TAILORINGS (German `search` folding ä to "ae",
    /// `-u-co-phonebk`) are likewise absent; `Collator/usage-de.js` is theirs.
    ///
    /// Shared with `String.prototype.localeCompare`, which ECMA-402 specifies as
    /// `Intl.Collator(locales, options).compare(this, that)` — so the two agree
    /// by construction rather than by two copies of the same approximation
    /// (`localeCompare/returns-same-results-as-Collator.js`).
    pub(crate) fn collator_compare(
        &mut self,
        resolved: u32,
        a: &str,
        b: &str,
    ) -> Result<f64, Thrown> {
        let work = a
            .len()
            .checked_add(b.len())
            .ok_or_else(|| Thrown("RangeError: native builtin iteration limit exceeded".into()))?;
        self.preflight_native_iteration_work(work as u64)?;
        let ignore_punct = self.intl_slot(resolved, "ignorePunctuation") == Value::bool(true);
        let sens = self.display(self.intl_slot(resolved, "sensitivity"));
        let upper_first = self.display(self.intl_slot(resolved, "caseFirst")) == "upper";
        let a_lengths = collation_key_lengths(a, ignore_punct)?;
        let b_lengths = collation_key_lengths(b, ignore_punct)?;
        let key_bytes = a_lengths
            .0
            .checked_add(a_lengths.1)
            .and_then(|n| n.checked_add(a_lengths.2))
            .and_then(|n| n.checked_add(b_lengths.0))
            .and_then(|n| n.checked_add(b_lengths.1))
            .and_then(|n| n.checked_add(b_lengths.2))
            .filter(|&n| n <= MAX_STRING_BYTES)
            .ok_or_else(|| Thrown("RangeError: Invalid string length".into()))?;
        self.preflight_guest_string_size(key_bytes)?;
        let (ka, kb) = (
            collation_key(a, ignore_punct, a_lengths)?,
            collation_key(b, ignore_punct, b_lengths)?,
        );
        let ord = match ka.0.cmp(&kb.0) {
            std::cmp::Ordering::Equal => {
                // Secondary (accents) is skipped by "base" and "case"; tertiary
                // (case) is skipped by "base" and "accent".
                let sec = if sens == "accent" || sens == "variant" {
                    ka.1.cmp(&kb.1)
                } else {
                    std::cmp::Ordering::Equal
                };
                if sec != std::cmp::Ordering::Equal {
                    sec
                } else if sens == "case" || sens == "variant" {
                    let t = ka.2.cmp(&kb.2);
                    if upper_first {
                        t.reverse()
                    } else {
                        t
                    }
                } else {
                    std::cmp::Ordering::Equal
                }
            }
            other => other,
        };
        Ok(match ord {
            std::cmp::Ordering::Less => -1.0,
            std::cmp::Ordering::Greater => 1.0,
            std::cmp::Ordering::Equal => 0.0,
        })
    }
}

/// The characters CLDR's root collation gives *variable* (shifted) weights —
/// whitespace, punctuation and general symbols. `Intl.Collator`'s
/// `ignorePunctuation` drops exactly these before comparing. Approximated from
/// std's Unicode predicates: anything that is neither a letter/number nor a
/// combining mark is variable. (Currency symbols and digits are variable in
/// UCA only above variable-top; without weight tables the distinction is not
/// observable through a code-point comparison.)
pub(crate) fn is_variable_collation_char(c: char) -> bool {
    !c.is_alphanumeric() && !unicode_normalization::char::is_combining_mark(c)
}

/// The (primary, secondary, tertiary) sort key `collator_compare` documents:
/// NFD, then split each scalar into its base-letter, accent and case
/// contributions. `ignore_punct` drops the variable characters first, so they
/// affect no level at all.
///
/// The case level records one flag per BASE character rather than per scalar of
/// the lowercased primary, because a full case mapping can change length
/// (U+0130 lowercases to two scalars) and the two levels must stay independent.
fn collation_key_lengths(s: &str, ignore_punct: bool) -> Result<(usize, usize, usize), Thrown> {
    use unicode_normalization::UnicodeNormalization;
    let mut primary = 0usize;
    let mut secondary = 0usize;
    let mut case = 0usize;
    for c in s.nfd() {
        if ignore_punct && is_variable_collation_char(c) {
            continue;
        }
        if unicode_normalization::char::is_combining_mark(c) {
            secondary = secondary
                .checked_add(c.len_utf8())
                .ok_or_else(|| Thrown("RangeError: Invalid string length".into()))?;
            continue;
        }
        for mapped in c.to_lowercase() {
            primary = primary
                .checked_add(mapped.len_utf8())
                .ok_or_else(|| Thrown("RangeError: Invalid string length".into()))?;
        }
        case = case
            .checked_add(1)
            .ok_or_else(|| Thrown("RangeError: Invalid string length".into()))?;
    }
    primary
        .checked_add(secondary)
        .and_then(|n| n.checked_add(case))
        .filter(|&n| n <= MAX_STRING_BYTES)
        .ok_or_else(|| Thrown("RangeError: Invalid string length".into()))?;
    Ok((primary, secondary, case))
}

fn collation_key(
    s: &str,
    ignore_punct: bool,
    lengths: (usize, usize, usize),
) -> Result<(String, String, Vec<u8>), Thrown> {
    use unicode_normalization::UnicodeNormalization;
    let mut primary = String::new();
    primary
        .try_reserve_exact(lengths.0)
        .map_err(|_| Thrown("RangeError: collation allocation failed".into()))?;
    let mut secondary = String::new();
    secondary
        .try_reserve_exact(lengths.1)
        .map_err(|_| Thrown("RangeError: collation allocation failed".into()))?;
    let mut case = Vec::new();
    case.try_reserve_exact(lengths.2)
        .map_err(|_| Thrown("RangeError: collation allocation failed".into()))?;
    for c in s.nfd() {
        if ignore_punct && is_variable_collation_char(c) {
            continue;
        }
        if unicode_normalization::char::is_combining_mark(c) {
            secondary.push(c);
            continue;
        }
        primary.extend(c.to_lowercase());
        case.push(u8::from(c.is_uppercase()));
    }
    debug_assert_eq!(primary.len(), lengths.0);
    debug_assert_eq!(secondary.len(), lengths.1);
    debug_assert_eq!(case.len(), lengths.2);
    Ok((primary, secondary, case))
}
