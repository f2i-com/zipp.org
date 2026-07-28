//! UTS-35 `unicode_locale_id` — structural parsing and canonicalization.
//!
//! This is the *grammar* half of ECMA-402's language-tag handling: it decides
//! whether a tag is well formed and puts its subtags into canonical order and
//! case. Everything here is derivable from the grammar alone; the registry
//! half — `iw`→`he`, `art-lojban`→`jbo`, `ru-SU`→`ru-RU` and likely subtags —
//! lives in `cldr_alias.rs` and runs after this, inside `canonicalize_tag`.
//!
//! Grammar implemented (UTS #35 §3.2):
//! ```text
//! unicode_locale_id  = unicode_language_id extensions* pu_extensions?
//! unicode_language_id= "root"
//!                    | unicode_language_subtag (sep script)? (sep region)? (sep variant)*
//! language_subtag    = alpha{2,3} | alpha{5,8}
//! script_subtag      = alpha{4}
//! region_subtag      = alpha{2} | digit{3}
//! variant_subtag     = alphanum{5,8} | digit alphanum{3}
//! extensions         = unicode_locale_ext | transformed_ext | other_ext
//! pu_extensions      = sep [xX] (sep alphanum{1,8})+
//! ```

/// A parsed `unicode_locale_id`. Every field is already canonical-cased and
/// canonically ordered, so `to_string` is a plain re-join.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LangTag {
    /// lowercase `unicode_language_subtag` ("und" for the implicit root).
    pub language: String,
    /// Titlecase script, or "".
    pub script: String,
    /// Uppercase alpha / 3-digit region, or "".
    pub region: String,
    /// Lowercase variants, sorted (UTS-35 canonical order).
    pub variants: Vec<String>,
    /// `-u-` attributes (lowercase, sorted, deduped).
    pub u_attributes: Vec<String>,
    /// `-u-` keywords as (key, value); sorted by key, a "true" value dropped.
    pub u_keywords: Vec<(String, String)>,
    /// True when the tag carried a `-u-` singleton at all (an empty `-u-` is
    /// only legal with at least one attribute or keyword, so this is only ever
    /// set alongside non-empty content — it keeps `-u-` printable when every
    /// keyword's value was dropped).
    pub has_u: bool,
    /// `-t-` extension body, canonical-cased ("" when absent).
    pub transform: String,
    /// Extensions other than `u`/`t`/`x`, as (singleton, body); sorted.
    pub other: Vec<(char, String)>,
    /// `-x-` private use body ("" when absent).
    pub private: String,
}

fn is_alpha(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphabetic())
}
fn is_digit(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}
fn is_alnum(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub(crate) fn is_language_subtag(s: &str) -> bool {
    is_alpha(s) && matches!(s.len(), 2 | 3 | 5 | 6 | 7 | 8)
}
pub(crate) fn is_script_subtag(s: &str) -> bool {
    is_alpha(s) && s.len() == 4
}
pub(crate) fn is_region_subtag(s: &str) -> bool {
    (is_alpha(s) && s.len() == 2) || (is_digit(s) && s.len() == 3)
}
pub(crate) fn is_variant_subtag(s: &str) -> bool {
    if is_alnum(s) && (5..=8).contains(&s.len()) {
        return true;
    }
    s.len() == 4 && s.as_bytes()[0].is_ascii_digit() && is_alnum(s)
}
/// A `-u-` / `-t-` keyword *value* (`type` in UTS-35): one or more 3..8
/// alphanumeric subtags. Also the shape ECMA-402 demands of the `calendar`,
/// `collation`, `numberingSystem` and `firstDayOfWeek` options.
pub(crate) fn is_type_sequence(s: &str) -> bool {
    !s.is_empty() && s.split('-').all(|p| (3..=8).contains(&p.len()) && is_alnum(p))
}
/// A `-u-` keyword key: exactly two alphanumerics whose second is a letter.
fn is_u_key(s: &str) -> bool {
    s.len() == 2 && s.as_bytes()[0].is_ascii_alphanumeric() && s.as_bytes()[1].is_ascii_alphabetic()
}
/// A `-u-` attribute: 3..8 alphanumerics (a keyless flag such as `-u-attrval-`).
fn is_u_attribute(s: &str) -> bool {
    (3..=8).contains(&s.len()) && is_alnum(s)
}

fn title_case(s: &str) -> String {
    let l = s.to_ascii_lowercase();
    format!("{}{}", l[..1].to_ascii_uppercase(), &l[1..])
}

impl LangTag {
    /// The `unicode_language_id` prefix — Intl.Locale's `baseName`.
    pub(crate) fn base_name(&self) -> String {
        let mut out = self.language.clone();
        if !self.script.is_empty() {
            out.push('-');
            out.push_str(&self.script);
        }
        if !self.region.is_empty() {
            out.push('-');
            out.push_str(&self.region);
        }
        for v in &self.variants {
            out.push('-');
            out.push_str(v);
        }
        out
    }

    /// The `-u-` extension body as it prints, or "" when there is nothing to
    /// print. A keyword whose value canonicalizes away (`kf-true` → `kf`) still
    /// prints its bare key.
    fn unicode_ext(&self) -> String {
        if !self.has_u {
            return String::new();
        }
        let mut parts: Vec<String> = vec!["u".to_string()];
        parts.extend(self.u_attributes.iter().cloned());
        for (k, v) in &self.u_keywords {
            parts.push(k.clone());
            if !v.is_empty() {
                parts.push(v.clone());
            }
        }
        parts.join("-")
    }

    /// The canonical tag string — Intl.Locale's `[[Locale]]`.
    pub(crate) fn canonical(&self) -> String {
        let mut out = self.base_name();
        // Extension singletons print in sorted order, `x-` always last.
        let mut exts: Vec<(char, String)> = self.other.clone();
        if !self.transform.is_empty() {
            exts.push(('t', self.transform.clone()));
        }
        let u = self.unicode_ext();
        if !u.is_empty() {
            exts.push(('u', u[2..].to_string()));
        }
        exts.sort_by_key(|(c, _)| *c);
        for (c, body) in exts {
            out.push('-');
            out.push(c);
            if !body.is_empty() {
                out.push('-');
                out.push_str(&body);
            }
        }
        if !self.private.is_empty() {
            out.push_str("-x-");
            out.push_str(&self.private);
        }
        out
    }

    /// Read a `-u-` keyword value. `Some("")` means "present with no value"
    /// (`-u-kn` — the canonical spelling of `-u-kn-true`).
    pub(crate) fn u_value(&self, key: &str) -> Option<&str> {
        self.u_keywords.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// Insert/replace a `-u-` keyword, keeping the list sorted by key. An empty
    /// value keeps the bare key; `remove` drops it entirely.
    pub(crate) fn set_u(&mut self, key: &str, value: Option<String>) {
        self.u_keywords.retain(|(k, _)| k != key);
        if let Some(v) = value {
            self.u_keywords.push((key.to_string(), v));
            self.u_keywords.sort_by(|a, b| a.0.cmp(&b.0));
            self.has_u = true;
        } else if self.u_keywords.is_empty() && self.u_attributes.is_empty() {
            self.has_u = false;
        }
    }
}

/// Canonicalize a `-u-` keyword value: lowercased, and the redundant explicit
/// `"true"` dropped (UTS-35 canonical form of `-u-kn-true` is `-u-kn`).
fn canon_u_value(v: &str) -> String {
    let l = v.to_ascii_lowercase();
    if l == "true" { String::new() } else { l }
}

/// Parse + canonicalize a `unicode_locale_id`. `None` when the tag is not
/// structurally valid (ECMA-402 IsStructurallyValidLanguageTag).
pub(crate) fn parse_lang_tag(tag: &str) -> Option<LangTag> {
    if tag.is_empty() || !tag.is_ascii() {
        return None;
    }
    let parts: Vec<&str> = tag.split('-').collect();
    if parts.iter().any(|p| p.is_empty() || p.len() > 8) {
        return None;
    }
    let mut t = LangTag::default();
    let mut i = 0usize;
    // ── unicode_language_id ──
    if parts[0].eq_ignore_ascii_case("root") {
        // UTS-35's `unicode_language_id` admits a literal "root", but ECMA-402's
        // IsStructurallyValidLanguageTag excludes exactly that alternative (it is
        // CLDR's spelling of "und", not BCP-47's) — `Intl.getCanonicalLocales`,
        // `new Intl.Locale` and `DisplayNames.prototype.of("root")` must each
        // throw a RangeError rather than answer "und".
        return None;
    } else if parts[0].len() == 1 {
        // An extension singleton in first place is not a language.
        return None;
    } else {
        if !is_language_subtag(parts[0]) {
            return None;
        }
        t.language = parts[0].to_ascii_lowercase();
        i = 1;
        if i < parts.len() && is_script_subtag(parts[i]) {
            t.script = title_case(parts[i]);
            i += 1;
        }
        if i < parts.len() && is_region_subtag(parts[i]) {
            t.region = if is_digit(parts[i]) {
                parts[i].to_string()
            } else {
                parts[i].to_ascii_uppercase()
            };
            i += 1;
        }
        while i < parts.len() && is_variant_subtag(parts[i]) {
            let v = parts[i].to_ascii_lowercase();
            // "de-DE-1996-1996" / "en-emodeng-emodeng": a repeated variant makes
            // the tag structurally invalid, not merely redundant.
            if t.variants.contains(&v) {
                return None;
            }
            t.variants.push(v);
            i += 1;
        }
        t.variants.sort();
    }
    // ── extensions ──
    let mut seen: Vec<char> = vec![];
    while i < parts.len() {
        if parts[i].len() != 1 {
            return None; // a non-singleton where an extension must start
        }
        let sing = parts[i].as_bytes()[0].to_ascii_lowercase() as char;
        if !sing.is_ascii_alphanumeric() {
            return None;
        }
        // "cmn-hans-cn-u-u": each singleton may appear at most once.
        if seen.contains(&sing) {
            return None;
        }
        seen.push(sing);
        i += 1;
        let start = i;
        // `pu_extensions = sep [xX] (sep alphanum{1,8})+` runs to the END of the
        // tag: a singleton-length subtag after `-x-` is private-use content, not
        // a new extension ("en-x-u-foo", "cmn-hans-cn-t-ca-u-ca-x-t-u").
        if sing == 'x' {
            i = parts.len();
        } else {
            while i < parts.len() && parts[i].len() != 1 {
                i += 1;
            }
        }
        let body = &parts[start..i];
        // "en-t" / "da-u": every extension needs at least one subtag.
        if body.is_empty() {
            return None;
        }
        match sing {
            'x' => {
                if !body.iter().all(|p| is_alnum(p)) {
                    return None;
                }
                t.private = body.iter().map(|p| p.to_ascii_lowercase()).collect::<Vec<_>>().join("-");
            }
            'u' => {
                parse_unicode_ext(body, &mut t)?;
                t.has_u = true;
            }
            't' => {
                t.transform = parse_transform_ext(body)?;
            }
            _ => {
                // other_extensions: (sep alphanum{2,8})+
                if !body.iter().all(|p| (2..=8).contains(&p.len()) && is_alnum(p)) {
                    return None;
                }
                t.other.push((
                    sing,
                    body.iter().map(|p| p.to_ascii_lowercase()).collect::<Vec<_>>().join("-"),
                ));
            }
        }
    }
    t.other.sort_by_key(|(c, _)| *c);
    Some(t)
}

/// `unicode_locale_extensions = "u" ((sep attribute)+ (sep keyword)* | (sep keyword)+)`
fn parse_unicode_ext(body: &[&str], t: &mut LangTag) -> Option<()> {
    let mut j = 0usize;
    while j < body.len() && !is_u_key(body[j]) {
        if !is_u_attribute(body[j]) {
            return None;
        }
        let a = body[j].to_ascii_lowercase();
        if !t.u_attributes.contains(&a) {
            t.u_attributes.push(a);
        }
        j += 1;
    }
    while j < body.len() {
        if !is_u_key(body[j]) {
            return None;
        }
        let key = body[j].to_ascii_lowercase();
        j += 1;
        let vstart = j;
        while j < body.len() && !is_u_key(body[j]) {
            if !is_u_attribute(body[j]) {
                return None;
            }
            j += 1;
        }
        let value = canon_u_value(&body[vstart..j].join("-"));
        // "da-u-ca-gregory-ca-buddhist": a repeated key keeps its FIRST value
        // (UTS-35 canonicalization drops the later duplicates).
        if !t.u_keywords.iter().any(|(k, _)| *k == key) {
            t.u_keywords.push((key, value));
        }
    }
    t.u_attributes.sort();
    t.u_keywords.sort_by(|a, b| a.0.cmp(&b.0));
    Some(())
}

/// `transformed_extensions = "t" ((sep tlang (sep tfield)*) | (sep tfield)+)`,
/// where `tfield = tkey tvalue` and `tkey = alpha digit`.
fn parse_transform_ext(body: &[&str]) -> Option<String> {
    let is_tkey =
        |s: &str| s.len() == 2 && s.as_bytes()[0].is_ascii_alphabetic() && s.as_bytes()[1].is_ascii_digit();
    let mut j = 0usize;
    let mut out: Vec<String> = vec![];
    if !is_tkey(body[0]) {
        // A tlang: a full unicode_language_id, with its own duplicate-variant
        // rule ("de-t-en-emodeng-emodeng" is invalid).
        let end = body.iter().position(|p| is_tkey(p)).unwrap_or(body.len());
        // `tlang` is spelled out in UTS-35 §3.7 WITHOUT the "root" alternative
        // that `unicode_language_id` allows, so "en-t-root" is a RangeError
        // even though "root" alone parses (transformed-ext-invalid.js).
        if body[0].eq_ignore_ascii_case("root") {
            return None;
        }
        let tlang = parse_lang_tag(&body[..end].join("-"))?;
        if !tlang.other.is_empty() || tlang.has_u || !tlang.private.is_empty() {
            return None;
        }
        out.push(tlang.base_name().to_ascii_lowercase());
        j = end;
    }
    let mut keys: Vec<String> = vec![];
    let mut fields: Vec<(String, String)> = vec![];
    while j < body.len() {
        if !is_tkey(body[j]) {
            return None;
        }
        let k = body[j].to_ascii_lowercase();
        if keys.contains(&k) {
            return None;
        }
        keys.push(k.clone());
        j += 1;
        let vstart = j;
        while j < body.len() && !is_tkey(body[j]) {
            if !((3..=8).contains(&body[j].len()) && is_alnum(body[j])) {
                return None;
            }
            j += 1;
        }
        if vstart == j {
            return None; // a tkey with no tvalue
        }
        fields.push((
            k,
            body[vstart..j].iter().map(|p| p.to_ascii_lowercase()).collect::<Vec<_>>().join("-"),
        ));
    }
    if out.is_empty() && fields.is_empty() {
        return None;
    }
    fields.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, v) in fields {
        out.push(k);
        out.push(v);
    }
    Some(out.join("-"))
}

/// The public entry the rest of the VM uses: CanonicalizeUnicodeLocaleId — the
/// grammar canonicalization above followed by `cldr_alias`'s registry
/// replacement (`iw`→`he`, `art-lojban`→`jbo`, `ru-SU`→`ru-RU`) — or `None` if
/// the tag is not structurally valid.
pub(crate) fn canonicalize_tag(tag: &str) -> Option<String> {
    parse_lang_tag(tag).map(|mut t| {
        crate::vm::cldr_alias::canonicalize(&mut t);
        t.canonical()
    })
}

/// `WeekdayToString(fw)` — ECMA-402 accepts the numeric weekday spellings for
/// `firstDayOfWeek` alongside the `-u-fw-` type codes.
pub(crate) fn weekday_to_string(fw: &str) -> String {
    match fw {
        "1" => "mon",
        "2" => "tue",
        "3" => "wed",
        "4" => "thu",
        "5" => "fri",
        "6" => "sat",
        "7" | "0" => "sun",
        other => other,
    }
    .to_string()
}

/// The `-u-fw-` code as the 1..7 (Monday..Sunday) integer `getWeekInfo` reports,
/// or `None` for a code that names no ISO weekday.
pub(crate) fn weekday_index(fw: &str) -> Option<i64> {
    Some(match fw {
        "mon" => 1,
        "tue" => 2,
        "wed" => 3,
        "thu" => 4,
        "fri" => 5,
        "sat" => 6,
        "sun" => 7,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_malformed_tags() {
        for bad in [
            "", "i", "x", "u", "419", "u-nu-latn-cu-bob", "hans-cmn-cn", "abcdefghi",
            "cmn-hans-cn-u-u", "cmn-hans-cn-t-u-ca-u", "de-gregory-gregory", "*", "de-*",
            "中文", "en-ß", "ıd", "en-emodeng-emodeng", "de-t-en-emodeng-emodeng", "en-t",
            "da-u", "en-US-u-ca-gregory-",
        ] {
            assert!(canonicalize_tag(bad).is_none(), "{bad} should be invalid");
        }
    }

    #[test]
    fn canonical_ordering() {
        assert_eq!(
            canonicalize_tag("de-latn-de-fonipa-1996-u-ca-gregory-co-phonebk-hc-h23-kf-true-kn-false-nu-latn")
                .as_deref(),
            Some("de-Latn-DE-1996-fonipa-u-ca-gregory-co-phonebk-hc-h23-kf-kn-false-nu-latn")
        );
        assert_eq!(canonicalize_tag("EN-us").as_deref(), Some("en-US"));
        assert_eq!(canonicalize_tag("da-u-ca-gregory-ca-buddhist").as_deref(), Some("da-u-ca-gregory"));
    }
}
