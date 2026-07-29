#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;
use crate::vm::{cldr_en, dtf_pattern};
use crate::vm::*;



/// IsValidDisplayNamesCode + CanonicalCodeForDisplayNames (ECMA-402 12.5.2):
/// validate `Intl.DisplayNames.prototype.of`'s argument against the instance's
/// `type` and return it in canonical case, or `None` for a RangeError. Pure
/// grammar — no display-name data is involved.
pub(crate) fn canonical_display_names_code(ty: &str, code: &str) -> Option<String> {
    match ty {
        // Step 1a: the code must match `unicode_language_id` — the language /
        // script / region / variant prefix ONLY. A tag carrying any extension
        // ("en-u-hebrew", "en-x-priv") parses fine as a `unicode_locale_id` but
        // is a RangeError here, so reject anything whose canonical form grows an
        // extension singleton.
        "language" => crate::vm::locale_tag::parse_lang_tag(code)
            .filter(|t| {
                !t.has_u
                    && t.transform.is_empty()
                    && t.other.is_empty()
                    && t.private.is_empty()
                    && t.u_keywords.is_empty()
            })
            .map(|t| t.canonical()),
        "region" => {
            let ok = (code.len() == 2 && code.bytes().all(|b| b.is_ascii_alphabetic()))
                || (code.len() == 3 && code.bytes().all(|b| b.is_ascii_digit()));
            ok.then(|| code.to_ascii_uppercase())
        }
        "script" => (code.len() == 4 && code.bytes().all(|b| b.is_ascii_alphabetic())).then(|| {
            let l = code.to_ascii_lowercase();
            format!("{}{}", l[..1].to_ascii_uppercase(), &l[1..])
        }),
        "currency" => (code.len() == 3 && code.bytes().all(|b| b.is_ascii_alphabetic()))
            .then(|| code.to_ascii_uppercase()),
        "calendar" => is_well_formed_type_code(code).then(|| {
            // CLDR's display-name table is keyed by the BCP-47 calendar name,
            // which for two calendars differs from ECMA-402's canonical id.
            // `Intl.supportedValuesOf("calendar")` reports the canonical form,
            // and `calendars-accepted-by-DisplayNames.js` requires every value
            // it reports to resolve here, so the alias has to be undone.
            match code.to_ascii_lowercase().as_str() {
                "ethioaa" => "ethiopic-amete-alem".to_string(),
                "islamicc" => "islamic-civil".to_string(),
                other => other.to_string(),
            }
        }),
        // dateTimeField takes one of the twelve field names, verbatim.
        _ => [
            "era", "year", "quarter", "month", "weekOfYear", "weekday", "day", "dayPeriod",
            "hour", "minute", "second", "timeZoneName",
        ]
        .contains(&code)
        .then(|| code.to_string()),
    }
}


/// A Unicode locale extension `type` value: 3-8 alphanumerics, optionally
/// repeated (`islamic-civil`). Used to range-check the `calendar` /
/// `numberingSystem` options before any data lookup (ECMA-402 IsWellFormed
/// CalendarCode / IsWellFormedNumberingSystemCode).
pub(crate) fn is_well_formed_type_code(s: &str) -> bool {
    !s.is_empty()
        && s.split('-').all(|p| {
            (3..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric())
        })
}
