//! UTS #35 §3.2.1 alias replacement and §4.3 likely subtags.
//!
//! `locale_tag.rs` is the *grammar* half of ECMA-402's tag handling; this is the
//! *registry* half. The tables it reads (`cldr_alias_data.rs`) are the
//! locale-INDEPENDENT part of CLDR — which spellings of a language, script,
//! region, variant, subdivision or `-u-`/`-t-` keyword type are retired, what
//! replaced them, and which script/region a bare language implies. There is no
//! translated content and no formatting pattern here, so shipping these tables
//! claims no locale support the engine does not have: `Intl.getCanonicalLocales`
//! and `Intl.Locale` are specified to canonicalize *every* well-formed tag,
//! including tags for locales no engine formats.
//!
//! The registries are indexed once, lazily, into maps keyed by the canonical
//! (lowercase language / Titlecase script / uppercase region) spellings
//! `LangTag` already stores, so the hot path is hash lookups.
//!
//! # Verified against node's ICU, value by value
//!
//! A corpus generated FROM the CLDR XML — every `languageAlias` type (plain and
//! with a script / region / variant / `-u-` / `-x-` tail), every
//! `territoryAlias` type under 16 language+script prefixes, every
//! `subdivisionAlias` type in both `-u-sd-` and `-u-rg-` position, every
//! `scriptAlias`/`variantAlias`, every bcp47 keyword alias on both sides of its
//! replacement, every `languageAlias` type again inside a `-t-` tlang, and every
//! likely-subtags `from` AND `to` maximized and minimized — was run through
//! `Intl.getCanonicalLocales` / `Intl.Locale.prototype.{maximize,minimize}` in
//! both engines: **29 922 comparisons, 22 disagreements (99.93 % agreement)**.
//! Every one of the 22 is a case where node is wrong, and each is reproducible:
//!
//! * **`-u-{ka,kf,kr,ks,kv}-yes` (5)** — node drops the `yes`. CLDR gives an
//!   `alias="yes"` only to the five boolean collation keys (`common/bcp47/
//!   collation.xml`), and `unicode-ext-canonicalize-yes-to-true.js` asserts
//!   these five tags round-trip unchanged. node fails that test.
//! * **`tw` → `ak`, `bh` → `bho` (2)** — node's `getCanonicalLocales` returns
//!   them unaliased while its own `new Intl.Locale("tw")` returns `ak`, and
//!   `getCanonicalLocales("TW")`/`("tw-GH")` return `ak`/`ak-GH`. Two code paths
//!   in one engine disagreeing settles it: the lowercase-bare-tag fast path is
//!   a V8 bug, and CLDR 47 says `<languageAlias type="tw" replacement="ak"/>`.
//! * **`sgn-NO` → `nsi` (5, incl. one inside `-t-`)** — node answers `nsl`,
//!   which appears in NO CLDR release and not in ICU 77.1's own
//!   `icu4c/source/data/misc/metadata.txt` (which carries `replacement{"nsi"}`).
//!   zipp ships what the table says.
//! * **`-u-tz-aqams` → `nzakl` (1)** — node answers `aqmcm`; CLDR 47's
//!   `common/bcp47/timezone.xml` says `<type name="aqams" deprecated="true"
//!   preferred="nzakl"/>`.
//! * **nine `und-…` minimizations** — node returns the input unchanged for
//!   `und-CW`, `und-PG`, `und-PH`, `und-PW`, `und-TK`, `und-TV`, `und-Arab-MM`,
//!   `und-Hant`, `und-Mong`, contradicting its own maximize: it maximizes
//!   `und-CW` to `pap-Latn-CW`, and `pap` maximizes to the same thing, so §4.3
//!   step 4's first trial succeeds and the answer is `pap`. ICU's own
//!   `LikelySubtags::minimizeSubtags` agrees with §4.3, and node minimizes
//!   `und-AT` to `de-AT` in exactly the same shape.
//!
//! Everything else — all 499 language rows, all 640 territory rows including the
//! 23 multi-valued ones resolved through likely subtags, all 147 live
//! subdivision rows, and all 7 745 likely-subtags rows in both directions —
//! matches node byte for byte.

use std::collections::HashMap;
use std::sync::OnceLock;

use super::cldr_alias_data as data;
use super::locale_tag::{self as lt, LangTag};

// ── indexing the blobs ──────────────────────────────────────────────────────

/// `key|value` records.
fn pairs(blob: &'static str) -> HashMap<&'static str, &'static str> {
    blob.lines().filter_map(|l| l.split_once('|')).collect()
}

/// `key|a|b` records, indexed by (key, a).
fn triples(blob: &'static str) -> HashMap<(&'static str, &'static str), &'static str> {
    blob.lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '|');
            Some(((it.next()?, it.next()?), it.next()?))
        })
        .collect()
}

/// `a b c` records, indexed by (a, b) → c … or by a → (b, c) when `two_keys` is
/// false. Both shapes appear in the likely-subtags tables.
fn likely1(blob: &'static str) -> HashMap<&'static str, (&'static str, &'static str)> {
    blob.lines()
        .filter_map(|l| {
            let mut it = l.split(' ');
            Some((it.next()?, (it.next()?, it.next()?)))
        })
        .collect()
}

fn likely2(blob: &'static str) -> HashMap<(&'static str, &'static str), &'static str> {
    blob.lines()
        .filter_map(|l| {
            let mut it = l.split(' ');
            Some(((it.next()?, it.next()?), it.next()?))
        })
        .collect()
}

/// One `languageAlias` row, split into the fields it matches on and the fields
/// it writes. `lang == "und"` in `from` is a wildcard that matches any language
/// (`und-arevela` retires the variant whatever the language is).
struct LangRule {
    from: Ids,
    to: Ids,
}

#[derive(Default)]
struct Ids {
    lang: String,
    script: String,
    region: String,
    variants: Vec<String>,
}

/// Split a `languageAlias` type/replacement into a `unicode_language_id`.
///
/// `None` for the rows whose type is a legacy tag that no well-formed
/// `unicode_locale_id` can spell — `i-ami`, `sgn-BE-FR`, `en-GB-oed`,
/// `zh-min-nan`, `zh-cmn-Hans` (a 3-letter extlang). They can never match, so
/// they are dropped at index time rather than carried as dead rules.
fn parse_ids(s: &str) -> Option<Ids> {
    let parts: Vec<&str> = s.split('-').collect();
    let mut i = 0;
    let mut out = Ids::default();
    if !lt::is_language_subtag(parts[0]) {
        return None;
    }
    out.lang = parts[0].to_ascii_lowercase();
    i += 1;
    if i < parts.len() && lt::is_script_subtag(parts[i]) {
        let l = parts[i].to_ascii_lowercase();
        out.script = format!("{}{}", l[..1].to_ascii_uppercase(), &l[1..]);
        i += 1;
    }
    if i < parts.len() && lt::is_region_subtag(parts[i]) {
        out.region = parts[i].to_ascii_uppercase();
        i += 1;
    }
    while i < parts.len() && lt::is_variant_subtag(parts[i]) {
        out.variants.push(parts[i].to_ascii_lowercase());
        i += 1;
    }
    if i != parts.len() {
        return None;
    }
    out.variants.sort();
    Some(out)
}

/// The `languageAlias` rules, most specific first.
///
/// The order is what makes `hy-arevmda` → `hyw` rather than `hy` — both
/// `hy-arevmda` and the wildcard `und-arevmda` match, and the rule that pins the
/// language must win. It follows UTS #35's "most specific match": rules that
/// match a variant outrank rules that do not, a pinned language outranks the
/// `und` wildcard, and script beats region.
fn lang_rules() -> &'static [LangRule] {
    static CELL: OnceLock<Vec<LangRule>> = OnceLock::new();
    CELL.get_or_init(|| {
        let mut v: Vec<LangRule> = data::LANGUAGE_ALIAS
            .lines()
            .filter_map(|l| l.split_once('|'))
            .filter_map(|(t, r)| {
                Some(LangRule { from: parse_ids(t)?, to: parse_ids(r)? })
            })
            .collect();
        v.sort_by_key(|r| {
            (
                r.from.variants.is_empty(),
                std::cmp::Reverse(r.from.variants.len()),
                r.from.lang == "und",
                r.from.script.is_empty(),
                r.from.region.is_empty(),
            )
        });
        v
    })
}

fn territory_alias() -> &'static HashMap<&'static str, &'static str> {
    static CELL: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    CELL.get_or_init(|| pairs(data::TERRITORY_ALIAS))
}

fn script_alias() -> &'static HashMap<&'static str, &'static str> {
    static CELL: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    CELL.get_or_init(|| pairs(data::SCRIPT_ALIAS))
}

fn variant_alias() -> &'static HashMap<&'static str, &'static str> {
    static CELL: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    CELL.get_or_init(|| pairs(data::VARIANT_ALIAS))
}

fn subdivision_alias() -> &'static HashMap<&'static str, &'static str> {
    static CELL: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    CELL.get_or_init(|| pairs(data::SUBDIVISION_ALIAS))
}

fn bcp47_alias() -> &'static HashMap<(&'static str, &'static str), &'static str> {
    static CELL: OnceLock<HashMap<(&'static str, &'static str), &'static str>> = OnceLock::new();
    CELL.get_or_init(|| triples(data::BCP47_TYPE_ALIAS))
}

struct Likely {
    lang: HashMap<&'static str, (&'static str, &'static str)>,
    lang_script: HashMap<(&'static str, &'static str), &'static str>,
    lang_region: HashMap<(&'static str, &'static str), &'static str>,
    und_script: HashMap<&'static str, (&'static str, &'static str)>,
    und_region: HashMap<&'static str, (&'static str, &'static str)>,
    und_script_region: HashMap<(&'static str, &'static str), &'static str>,
}

fn likely() -> &'static Likely {
    static CELL: OnceLock<Likely> = OnceLock::new();
    CELL.get_or_init(|| Likely {
        lang: likely1(data::LIKELY_LANG),
        lang_script: likely2(data::LIKELY_LANG_SCRIPT),
        lang_region: likely2(data::LIKELY_LANG_REGION),
        und_script: likely1(data::LIKELY_UND_SCRIPT),
        und_region: likely1(data::LIKELY_UND_REGION),
        und_script_region: likely2(data::LIKELY_UND_SCRIPT_REGION),
    })
}

// ── §3.2.1 alias replacement ────────────────────────────────────────────────

/// Apply `rule` to a language id that has already been checked to match it.
/// Returns whether anything changed — the caller loops to a fixed point, so a
/// rule that would rewrite a tag to itself must not report progress.
///
/// The two asymmetries here are both observable. A field the rule MATCHED on is
/// consumed even when the replacement leaves it empty (`sgn-BR` → `bzs`, not
/// `bzs-BR`); a field the rule did NOT match on is only filled in when the tag
/// lacks it (`sh` → `sr-Latn`, but `sh-Arab-AQ` → `sr-Arab-AQ`).
fn apply_rule(r: &LangRule, t: &mut LangTag) -> bool {
    let mut changed = false;
    for v in &r.from.variants {
        if let Some(i) = t.variants.iter().position(|x| x == v) {
            t.variants.remove(i);
            changed = true;
        }
    }
    if r.to.lang != "und" && t.language != r.to.lang {
        t.language = r.to.lang.clone();
        changed = true;
    }
    for (matched, repl, field) in [
        (&r.from.script, &r.to.script, &mut t.script),
        (&r.from.region, &r.to.region, &mut t.region),
    ] {
        let next = if matched.is_empty() && !field.is_empty() { None } else { Some(repl.clone()) };
        if let Some(n) = next {
            if *field != n {
                *field = n;
                changed = true;
            }
        }
    }
    for v in &r.to.variants {
        if !t.variants.contains(v) {
            t.variants.push(v.clone());
            changed = true;
        }
    }
    t.variants.sort();
    changed
}

fn rule_matches(r: &LangRule, t: &LangTag) -> bool {
    (r.from.lang == "und" || r.from.lang == t.language)
        && (r.from.script.is_empty() || r.from.script == t.script)
        && (r.from.region.is_empty() || r.from.region == t.region)
        && r.from.variants.iter().all(|v| t.variants.contains(v))
}

/// The likely REGION for a language id with its region removed — step 2 of
/// UTS #35's multi-valued `territoryAlias` rule ("look up the most likely
/// territory for the base language code, and script if there is one").
fn likely_region(lang: &str, script: &str) -> Option<String> {
    let probe = LangTag {
        language: lang.to_string(),
        script: script.to_string(),
        ..LangTag::default()
    };
    add_likely_subtags(&probe).map(|m| m.region)
}

fn replace_territory(t: &mut LangTag) -> bool {
    if t.region.is_empty() {
        return false;
    }
    let Some(repl) = territory_alias().get(t.region.as_str()) else { return false };
    let list: Vec<&str> = repl.split(' ').collect();
    let pick = if list.len() == 1 {
        list[0].to_string()
    } else {
        // The tag's OWN region is deliberately not consulted: "az-NT" resolves
        // through az → az-Latn-AZ, and because AZ is not one of [SA, IQ] the
        // answer is the list head SA even though an `az_IQ` likely-subtag exists.
        match likely_region(&t.language, &t.script) {
            Some(r) if list.contains(&r.as_str()) => r,
            _ => list[0].to_string(),
        }
    };
    if pick == t.region {
        return false;
    }
    t.region = pick;
    true
}

/// CanonicalizeUnicodeLocaleId's alias half (UTS #35 §3.2.1), in place.
/// `locale_tag.rs` has already put the tag in canonical syntax.
pub(crate) fn canonicalize(t: &mut LangTag) {
    canonicalize_language_id(t);
    // `-u-` keyword types: `<type name=N alias=A/>` and the retired-spelling
    // rows, then §3.2.1's "any type value `true` is removed" (`kb-yes` is
    // `kb-true` is a bare `kb`). `sd`/`rg` name a subdivision, not a keyword
    // type, so they read the subdivision registry instead.
    for (k, v) in t.u_keywords.iter_mut() {
        if v.is_empty() {
            continue;
        }
        if k == "sd" || k == "rg" {
            if let Some(r) = subdivision_alias().get(v.as_str()) {
                // A row may retire a subdivision in favour of a whole REGION
                // (`<subdivisionAlias type="fi01" replacement="AX"/>`, Åland
                // having become a country). A `unicode_subdivision_id` is
                // `region + suffix` (UTS #35 §3.6.5) and `zzzz` is the
                // whole-region suffix, so "AX" is spelled `axzzzz` here —
                // "und-u-rg-fi01" → "und-u-rg-axzzzz", as ICU also produces.
                let is_region = (r.len() == 2 && r.bytes().all(|b| b.is_ascii_alphabetic()))
                    || (r.len() == 3 && r.bytes().all(|b| b.is_ascii_digit()));
                *v = if is_region {
                    format!("{}zzzz", r.to_ascii_lowercase())
                } else {
                    r.to_ascii_lowercase()
                };
            }
            continue;
        }
        if let Some(r) = bcp47_alias().get(&(k.as_str(), v.as_str())) {
            *v = r.to_string();
        }
        if v == "true" {
            v.clear();
        }
    }
    canonicalize_transform(t);
}

/// The language-id half, looped to a fixed point the way ICU's alias replacer
/// does: replacing a language can expose a retired region ("cnr" → "sr-ME"),
/// and replacing a region can expose nothing further, so a bounded loop is both
/// necessary and sufficient. The cap only guards against a future table that
/// contains a cycle.
fn canonicalize_language_id(t: &mut LangTag) {
    for _ in 0..8 {
        let mut changed = false;
        if let Some(r) = lang_rules().iter().find(|r| rule_matches(r, t)) {
            changed = apply_rule(r, t);
        }
        if !changed {
            changed = replace_territory(t);
        }
        if !changed && !t.script.is_empty() {
            if let Some(s) = script_alias().get(t.script.as_str()) {
                if t.script != *s {
                    t.script = s.to_string();
                    changed = true;
                }
            }
        }
        if !changed {
            for v in t.variants.iter_mut() {
                if let Some(r) = variant_alias().get(v.as_str()) {
                    if v != r {
                        *v = r.to_string();
                        changed = true;
                    }
                }
            }
            if changed {
                t.variants.sort();
            }
        }
        if !changed {
            return;
        }
    }
}

/// The `-t-` extension: its `tlang` is a `unicode_language_id` and gets the same
/// alias treatment ("en-t-iw" → "en-t-he"), and each `tfield` value is a bcp47
/// type ("m0-names" → "m0-prprname"). A `true` tvalue is NOT dropped — `tkey`
/// must be followed by a `tvalue`, so there is nothing to fall back to.
fn canonicalize_transform(t: &mut LangTag) {
    if t.transform.is_empty() {
        return;
    }
    let parts: Vec<String> = t.transform.split('-').map(str::to_string).collect();
    let is_tkey = |s: &str| {
        s.len() == 2 && s.as_bytes()[0].is_ascii_alphabetic() && s.as_bytes()[1].is_ascii_digit()
    };
    let first_key = parts.iter().position(|p| is_tkey(p)).unwrap_or(parts.len());
    let mut out: Vec<String> = Vec::new();
    if first_key > 0 {
        if let Some(mut tl) = lt::parse_lang_tag(&parts[..first_key].join("-")) {
            canonicalize_language_id(&mut tl);
            out.push(tl.base_name().to_ascii_lowercase());
        } else {
            out.push(parts[..first_key].join("-"));
        }
    }
    let mut i = first_key;
    while i < parts.len() {
        let key = parts[i].clone();
        let start = i + 1;
        let mut end = start;
        while end < parts.len() && !is_tkey(&parts[end]) {
            end += 1;
        }
        let value = parts[start..end].join("-");
        let value = bcp47_alias()
            .get(&(key.as_str(), value.as_str()))
            .map(|s| s.to_string())
            .unwrap_or(value);
        out.push(key);
        out.push(value);
        i = end;
    }
    t.transform = out.join("-");
}

// ── §4.3 likely subtags ─────────────────────────────────────────────────────

/// AddLikelySubtags. `None` when no table row covers the tag at all, which is
/// the specified failure mode and NOT the same as "expand to the root": an
/// unlisted language keeps its own identity (`Intl.Locale("xtg").maximize()` is
/// "xtg", per `Locale/likely-subtags-grandfathered.js`).
pub(crate) fn add_likely_subtags(t: &LangTag) -> Option<LangTag> {
    let l = likely();
    let (lang, script, region) = (t.language.as_str(), t.script.as_str(), t.region.as_str());
    let und = lang.is_empty() || lang == "und";
    // Lookup order. ICU stores the table as a language→script→region trie and
    // falls back a level at a time, so a matched SCRIPT outranks a matched
    // region: `und-Cpmn-CY` resolves through `und_Cpmn` (whose target language
    // is itself `und`) and stays `und-Cpmn-CY`, rather than through `und_CY`,
    // which would wrongly make it Greek. A language that appears in no row at
    // all fails outright — `xtg` must not inherit the root's `en-Latn-US`.
    let hit: Option<(String, String, String)> = None
        .or_else(|| {
            (!und && !script.is_empty())
                .then(|| l.lang_script.get(&(lang, script)).map(|r| {
                    (lang.to_string(), script.to_string(), r.to_string())
                }))
                .flatten()
        })
        .or_else(|| {
            (!und && !region.is_empty())
                .then(|| l.lang_region.get(&(lang, region)).map(|s| {
                    (lang.to_string(), s.to_string(), region.to_string())
                }))
                .flatten()
        })
        .or_else(|| {
            (!und).then(|| l.lang.get(lang).map(|(s, r)| {
                (lang.to_string(), s.to_string(), r.to_string())
            }))
            .flatten()
        })
        .or_else(|| {
            (und && !script.is_empty() && !region.is_empty())
                .then(|| l.und_script_region.get(&(script, region)).map(|la| {
                    (la.to_string(), script.to_string(), region.to_string())
                }))
                .flatten()
        })
        .or_else(|| {
            (!script.is_empty())
                .then(|| l.und_script.get(script).map(|(la, r)| {
                    (la.to_string(), script.to_string(), r.to_string())
                }))
                .flatten()
        })
        .or_else(|| {
            (und && !region.is_empty())
                .then(|| l.und_region.get(region).map(|(la, s)| {
                    (la.to_string(), s.to_string(), region.to_string())
                }))
                .flatten()
        })
        .or_else(|| {
            let (la, s, r) = data::LIKELY_UND;
            und.then(|| (la.to_string(), s.to_string(), r.to_string()))
        });
    let (mlang, mscript, mregion) = hit?;
    let mut out = t.clone();
    // The original's own subtags win over the match's; only the gaps are filled.
    out.language = if und { mlang } else { t.language.clone() };
    out.script = if script.is_empty() { mscript } else { t.script.clone() };
    out.region = if region.is_empty() { mregion } else { t.region.clone() };
    Some(out)
}

/// RemoveLikelySubtags: the shortest tag that maximizes back to the same thing.
pub(crate) fn remove_likely_subtags(t: &LangTag) -> Option<LangTag> {
    let max = add_likely_subtags(t)?;
    for (script, region) in [
        ("", ""),
        ("", max.region.as_str()),
        (max.script.as_str(), ""),
    ] {
        let trial = LangTag {
            language: max.language.clone(),
            script: script.to_string(),
            region: region.to_string(),
            ..LangTag::default()
        };
        if let Some(m) = add_likely_subtags(&trial) {
            if m.language == max.language && m.script == max.script && m.region == max.region {
                let mut out = t.clone();
                out.language = trial.language;
                out.script = trial.script;
                out.region = trial.region;
                return Some(out);
            }
        }
    }
    let mut out = t.clone();
    out.language = max.language;
    out.script = max.script;
    out.region = max.region;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon(tag: &str) -> String {
        let mut t = lt::parse_lang_tag(tag).expect("well formed");
        canonicalize(&mut t);
        t.canonical()
    }

    fn maximize(tag: &str) -> String {
        let t = lt::parse_lang_tag(tag).unwrap();
        add_likely_subtags(&t).unwrap_or(t).canonical()
    }

    fn minimize(tag: &str) -> String {
        let t = lt::parse_lang_tag(tag).unwrap();
        remove_likely_subtags(&t).unwrap_or(t).canonical()
    }

    /// Every expectation below was read off node 24.12 (ICU 77.1, CLDR 47) —
    /// the same CLDR release `cldr_alias_data.rs` is generated from.
    #[test]
    fn matches_icu_on_the_shapes_that_differ() {
        for (input, want) in [
            // A matched region is consumed; an unmatched one survives.
            ("sgn-BR", "bzs"),
            ("sgn-BR-fonipa", "bzs-fonipa"),
            ("sh", "sr-Latn"),
            ("sh-Cyrl", "sr-Cyrl"),
            ("sh-Arab-AQ", "sr-Arab-AQ"),
            // A pinned-language variant rule outranks the `und` wildcard.
            ("hy-arevmda", "hyw"),
            ("hy-arevela", "hy"),
            ("aa-saaho", "ssy"),
            ("ja-Latn-hepburn-heploc", "ja-Latn-alalc97"),
            ("fi-aaland", "fi-AX"),
            // Multi-valued territoryAlias, resolved through likely subtags.
            ("ru-SU", "ru-RU"),
            ("und-SU", "und-RU"),
            ("hy-SU", "hy-AM"),
            ("und-Armn-810", "und-Armn-AM"),
            ("sr-Latn-CS", "sr-Latn-RS"),
            ("az-NT", "az-SA"),
            // Chained: language rule exposes a region that needs no further work.
            ("cnr", "sr-ME"),
            ("prs", "fa-AF"),
            // Registries that are not the language one.
            ("en-Qaai", "en-Zinh"),
            ("de-polytoni", "de-polyton"),
            ("und-u-ca-islamicc", "und-u-ca-islamic-civil"),
            ("und-u-ms-imperial", "und-u-ms-uksystem"),
            ("und-u-tz-zulu", "und-u-tz-utc"),
            ("und-u-rg-no23", "und-u-rg-no50"),
            ("und-NO-u-sd-cn11", "und-NO-u-sd-cnbj"),
            ("en-t-iw", "en-t-he"),
            ("und-Latn-t-und-hani-m0-names", "und-Latn-t-und-hani-m0-prprname"),
            // Not aliased — the rows CLDR keeps COMMENTED OUT must stay out.
            ("sr", "sr"),
            ("qaa", "qaa"),
            ("xtg", "xtg"),
        ] {
            assert_eq!(canon(input), want, "canonicalize({input})");
        }
    }

    /// `-u-kb-yes` → `-u-kb` for the five keys CLDR gives a `yes` alias, and for
    /// no others. This is a deliberate disagreement with V8/ICU, which drops the
    /// value for `ka`/`kf`/`kr`/`ks`/`kv` too; `unicode-ext-canonicalize-yes-to-true.js`
    /// asserts those five tags round-trip unchanged, and CLDR's `collation.xml`
    /// only carries `alias="yes"` on the boolean keys.
    #[test]
    fn yes_is_true_only_where_cldr_says_so() {
        for k in ["kb", "kc", "kh", "kk", "kn"] {
            assert_eq!(canon(&format!("und-u-{k}-yes")), format!("und-u-{k}"));
        }
        for k in ["ka", "kf", "kr", "ks", "kv"] {
            assert_eq!(canon(&format!("und-u-{k}-yes")), format!("und-u-{k}-yes"));
        }
    }

    #[test]
    fn likely_subtags_round_trip() {
        for (input, max, min) in [
            ("en", "en-Latn-US", "en"),
            ("en-Shaw", "en-Shaw-GB", "en-Shaw"),
            ("en-Arab", "en-Arab-US", "en-Arab"),
            ("it-Kana-CA", "it-Kana-CA", "it-Kana-CA"),
            ("und", "en-Latn-US", "en"),
            ("und-Thai", "th-Thai-TH", "th"),
            ("und-419", "es-Latn-419", "es-419"),
            ("und-150", "en-Latn-150", "en-150"),
            ("und-AT", "de-Latn-AT", "de-AT"),
            ("und-Cyrl-RO", "bg-Cyrl-RO", "bg-RO"),
            ("und-AQ", "en-Latn-AQ", "en-AQ"),
            // No row anywhere: the tag is its own maximum and minimum.
            ("xtg", "xtg", "xtg"),
        ] {
            assert_eq!(maximize(input), max, "maximize({input})");
            assert_eq!(minimize(input), min, "minimize({input})");
        }
    }
}
