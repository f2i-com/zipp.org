#![allow(unused_imports)]
use super::*;
use crate::bytecode::{Instr, Program, UpvalSource};
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
        let numeric = self.intl_slot(resolved, "numeric") == Value::bool(true);
        let opts = KeyOptions {
            ignore_punct,
            numeric,
        };
        let a_lengths = collation_key_lengths(a, opts)?;
        let b_lengths = collation_key_lengths(b, opts)?;
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
            collation_key(a, opts, a_lengths)?,
            collation_key(b, opts, b_lengths)?,
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

/// The resolved options that shape a sort key.
#[derive(Clone, Copy)]
struct KeyOptions {
    ignore_punct: bool,
    /// `numeric: true` / `-u-kn`: a run of decimal digits is ONE primary
    /// element ordered by its numeric value (CLDR/UTS #10 numeric collation).
    numeric: bool,
}

/// The value of a decimal digit (Unicode `Nd`) — ASCII, or any digit of the
/// ECMA-402 Table 10 numbering systems, whose ten digits are consecutive.
fn numeric_digit(c: char) -> Option<u8> {
    if c.is_ascii_digit() {
        return Some(c as u8 - b'0');
    }
    if (c as u32) < 0x660 {
        return None;
    }
    // `hanidec`'s digits are not consecutive (its "zero" U+3007 is followed by
    // CJK brackets), so it is not a range.
    NUMBERING_SYSTEM_ZERO
        .iter()
        .find(|(name, zero)| *name != "hanidec" && (*zero..zero + 10).contains(&(c as u32)))
        .map(|(_, zero)| (c as u32 - zero) as u8)
}

/// The primary-key encoding of one digit run under numeric collation: a `'0'`
/// marker (so the run sorts where a digit would against any other character),
/// the significant-digit count as eight fixed-width `A`..`P` nibbles, then the
/// significant digits. Two runs therefore compare by length and then by digits,
/// i.e. by value, and leading zeros are ignored at every level ("a01" equals
/// "a1", as in ICU). `digits` holds the run's digit values.
fn push_numeric_run(primary: &mut String, digits: &[u8]) {
    let sig = match digits.iter().position(|d| *d != 0) {
        Some(i) => &digits[i..],
        None => &[0][..],
    };
    primary.push('0');
    let n = sig.len() as u32;
    for shift in (0..8).rev() {
        primary.push((b'A' + ((n >> (shift * 4)) & 0xF) as u8) as char);
    }
    primary.extend(sig.iter().map(|d| (b'0' + d) as char));
}

/// The encoded byte length of a digit run (see `push_numeric_run`).
fn numeric_run_len(digits: &[u8]) -> usize {
    let sig = digits
        .iter()
        .position(|d| *d != 0)
        .map_or(1, |i| digits.len() - i);
    9 + sig
}

/// The (primary, secondary, tertiary) sort key `collator_compare` documents:
/// NFD, then split each scalar into its base-letter, accent and case
/// contributions. `ignore_punct` drops the variable characters first, so they
/// affect no level at all.
///
/// The case level records one flag per BASE character rather than per scalar of
/// the lowercased primary, because a full case mapping can change length
/// (U+0130 lowercases to two scalars) and the two levels must stay independent.
/// Under `numeric` a digit run is one primary element with no case flag.
fn collation_key_lengths(s: &str, opts: KeyOptions) -> Result<(usize, usize, usize), Thrown> {
    use unicode_normalization::UnicodeNormalization;
    let mut primary = 0usize;
    let mut secondary = 0usize;
    let mut case = 0usize;
    let mut run: Vec<u8> = Vec::new();
    for c in s.nfd() {
        if opts.numeric {
            if let Some(d) = numeric_digit(c) {
                run.push(d);
                continue;
            }
            if !run.is_empty() {
                primary = primary
                    .checked_add(numeric_run_len(&run))
                    .ok_or_else(|| Thrown("RangeError: Invalid string length".into()))?;
                run.clear();
            }
        }
        if opts.ignore_punct && is_variable_collation_char(c) {
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
    if !run.is_empty() {
        primary = primary
            .checked_add(numeric_run_len(&run))
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
    opts: KeyOptions,
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
    let mut run: Vec<u8> = Vec::new();
    for c in s.nfd() {
        if opts.numeric {
            if let Some(d) = numeric_digit(c) {
                run.push(d);
                continue;
            }
            if !run.is_empty() {
                push_numeric_run(&mut primary, &run);
                run.clear();
            }
        }
        if opts.ignore_punct && is_variable_collation_char(c) {
            continue;
        }
        if unicode_normalization::char::is_combining_mark(c) {
            secondary.push(c);
            continue;
        }
        primary.extend(c.to_lowercase());
        case.push(u8::from(c.is_uppercase()));
    }
    if !run.is_empty() {
        push_numeric_run(&mut primary, &run);
    }
    debug_assert_eq!(primary.len(), lengths.0);
    debug_assert_eq!(secondary.len(), lengths.1);
    debug_assert_eq!(case.len(), lengths.2);
    Ok((primary, secondary, case))
}
