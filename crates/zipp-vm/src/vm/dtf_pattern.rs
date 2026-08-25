//! UTS #35 date-time pattern selection and interpretation over the CLDR `en`
//! tables in `cldr_en`.
//!
//! ECMA-402's DateTimeFormat resolves a set of *components* (year, month,
//! weekday, … each with a width) into a locale PATTERN, then renders that
//! pattern. zipp used to skip the middle step and emit `M/D/Y, HH:MM:SS`
//! unconditionally, which is why every `en` month name, weekday name, era and
//! `dateStyle` came out wrong. This module is the middle step:
//!
//!   * a `Request` is the resolved components as UTS #35 pattern letters —
//!     the skeleton, the canonical spelling of "which fields, how wide";
//!   * `best_pattern_halves` matches that against CLDR's `availableFormats`
//!     (UTS #35 §4.5 "Matching Skeletons"), adjusts the winner's field widths
//!     to the ones actually asked for, and attaches anything the winner lacks
//!     with `appendItems`;
//!   * `interval_pattern` does the same against `intervalFormats` for
//!     `formatRange`;
//!   * `parse_pattern` splits a pattern into literal runs and fields so the
//!     caller can emit one typed part per field.
//!
//! Nothing here is locale-general — it reads `cldr_en` and formats `en`. That
//! is the whole point: `[[AvailableLocales]]` is `["en", "en-US"]`.

use crate::vm::cldr_en as d;

/// One piece of a CLDR pattern: a literal run, or `count` repetitions of a
/// field letter.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Item {
    Lit(String),
    Field(char, usize),
}

/// Split a CLDR pattern into literals and fields.
///
/// `'` quotes a literal run (`'at'`, `'week' W 'of' MMMM`) and `''` is one
/// apostrophe; missing that quoting would render "at" as a-then-t fields.
pub(crate) fn parse_pattern(p: &str) -> Vec<Item> {
    let mut out: Vec<Item> = vec![];
    let mut lit = String::new();
    let mut it = p.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\'' {
            if it.peek() == Some(&'\'') {
                it.next();
                lit.push('\'');
                continue;
            }
            for q in it.by_ref() {
                if q == '\'' {
                    break;
                }
                lit.push(q);
            }
            continue;
        }
        if c.is_ascii_alphabetic() {
            if !lit.is_empty() {
                out.push(Item::Lit(std::mem::take(&mut lit)));
            }
            let mut n = 1;
            while it.peek() == Some(&c) {
                it.next();
                n += 1;
            }
            out.push(Item::Field(c, n));
            continue;
        }
        lit.push(c);
    }
    if !lit.is_empty() {
        out.push(Item::Lit(lit));
    }
    out
}

/// The field CLASS a pattern letter belongs to, in UTS #35 skeleton order.
/// Matching compares classes, not letters, so a request for `z` (specific
/// non-location zone) can reuse the `hmv` (generic zone) pattern and then swap
/// the letter back — which is what ICU does and what makes
/// `{hour, minute, timeZoneName}` resolve at all.
fn class_of(c: char) -> Option<u8> {
    Some(match c {
        'G' => 0,
        'y' | 'Y' | 'u' | 'U' | 'r' => 1,
        'Q' | 'q' => 2,
        'M' | 'L' => 3,
        'w' | 'W' => 4,
        'd' | 'D' | 'F' | 'g' => 5,
        'E' | 'e' | 'c' => 6,
        'a' | 'b' | 'B' => 7,
        'h' | 'H' | 'K' | 'k' | 'j' => 8,
        'm' => 9,
        's' => 10,
        'S' | 'A' => 11,
        'z' | 'Z' | 'O' | 'v' | 'V' | 'X' | 'x' => 12,
        _ => return None,
    })
}

/// `class_of` for callers outside this module (the range formatter ranks the
/// differing fields by significance).
pub(crate) fn class_of_pub(c: char) -> Option<u8> {
    class_of(c)
}

/// Where an interval pattern splits, and which of its items belong to which
/// endpoint. Returns `(separator index, first ranged index, last ranged index)`.
///
/// CLDR writes an interval pattern by naming the DIFFERING fields twice and the
/// shared ones once: `MMM d – d, y` ranges only the day. Everything outside the
/// two ranged runs is common to both endpoints and is reported `shared`, which
/// is what `formatRangeToParts` asserts ("Jan" and "2019" shared, the two days
/// start/endRange).
pub(crate) fn interval_layout(items: &[Item]) -> Option<(usize, usize, usize)> {
    let mut count = [0u8; 16];
    for it in items {
        if let Item::Field(c, _) = it {
            if let Some(k) = class_of(*c) {
                count[k as usize] += 1;
            }
        }
    }
    let repeated = |it: &Item| matches!(it, Item::Field(c, _) if class_of(*c).is_some_and(|k| count[k as usize] > 1));
    // The SECOND occurrence of a repeated class opens the end endpoint.
    let mut seen = [false; 16];
    let mut second = None;
    for (i, it) in items.iter().enumerate() {
        let Item::Field(c, _) = it else { continue };
        let Some(k) = class_of(*c) else { continue };
        if seen[k as usize] {
            second = Some(i);
            break;
        }
        seen[k as usize] = true;
    }
    let second = second?;
    // The separator is the literal immediately before that second occurrence.
    let sep = (0..second)
        .rev()
        .find(|i| matches!(items[*i], Item::Lit(_)))?;
    let first_ranged = (0..sep).find(|i| repeated(&items[*i]))?;
    let last_ranged = (sep + 1..items.len())
        .rev()
        .find(|i| repeated(&items[*i]))?;
    Some((sep, first_ranged, last_ranged))
}

/// `splice_glue` on already-RENDERED parts, for the range formatter: `{1}` takes
/// the shared date, `{0}` the ranged time, and the glue's own literals are
/// shared by both endpoints.
pub(crate) fn splice_glue_parts(
    glue: &str,
    date: &[(String, String, &'static str)],
    time: &[(String, String, &'static str)],
) -> Vec<(String, String, &'static str)> {
    let mut out = vec![];
    for it in splice_glue(glue, &[Item::Field('\u{1}', 1)], &[Item::Field('\u{2}', 1)]) {
        match it {
            Item::Field('\u{1}', _) => out.extend(date.iter().cloned()),
            Item::Field('\u{2}', _) => out.extend(time.iter().cloned()),
            Item::Lit(s) => out.push(("literal".to_string(), s, "shared")),
            _ => {}
        }
    }
    out
}

/// Classes 0-6 are the date half, 7-12 the time half. CLDR stores the two
/// halves separately (`availableFormats` has `yMMMd` and `hms`, never
/// `yMMMdhms`) and joins them with `dateTimeFormats`.
fn is_date_class(k: u8) -> bool {
    k <= 6
}

/// The skeleton fields of a pattern, as `(class, letter, count)` — the pattern's
/// own spelling, which is what the width adjustment below rewrites.
fn pattern_fields(p: &str) -> Vec<(u8, char, usize)> {
    parse_pattern(p)
        .into_iter()
        .filter_map(|i| match i {
            Item::Field(c, n) => class_of(c).map(|k| (k, c, n)),
            _ => None,
        })
        .collect()
}

/// The resolved DateTimeFormat components, as CLDR pattern letters. Each entry
/// is the exact field the request asks for; `skeleton_for` orders them and
/// `best_pattern_halves` finds the locale pattern that carries them.
#[derive(Default, Clone)]
pub(crate) struct Request {
    pub fields: Vec<(char, usize)>,
}

impl Request {
    pub fn push(&mut self, c: char, n: usize) {
        self.fields.push((c, n));
    }
}

/// `(month width, has weekday)` decides which of CLDR's four `dateTimeFormats`
/// glues joins a date pattern to a time pattern, exactly as ICU4C's
/// `DateTimePatternGenerator::getBestPattern` does: a wide month makes it
/// "long" (and "full" when a weekday is present too), an abbreviated month
/// "medium", anything else "short". For `en` that is the difference between
/// "May 1, 1886 at 2:12 PM" and "5/1/1886, 2:12 PM".
fn glue_index(date_pattern: &str) -> usize {
    let f = pattern_fields(date_pattern);
    let month = f
        .iter()
        .find(|(k, ..)| *k == 3)
        .map(|(_, _, n)| *n)
        .unwrap_or(0);
    let weekday = f.iter().any(|(k, ..)| *k == 6);
    match month {
        4.. if weekday => 0, // full
        4.. => 1,            // long
        3 => 2,              // medium
        _ => 3,              // short
    }
}

/// UTS #35 §4.5: rewrite `pattern`'s field widths to the requested ones.
///
/// A count change WITHIN a class is free (`M` → `MMMM`), and so is a letter
/// change within the zone class. The one thing that is not rewritten is a field
/// the request does not name — those stay as the locale wrote them.
fn adjust_widths(pattern: &str, req: &Request) -> String {
    let want: Vec<(u8, char, usize)> = req
        .fields
        .iter()
        .filter_map(|(c, n)| class_of(*c).map(|k| (k, *c, *n)))
        .collect();
    let mut out = String::new();
    for item in parse_pattern(pattern) {
        match item {
            Item::Lit(s) => {
                // Re-quote: a literal that would otherwise re-parse as fields
                // (`at`, `week`) has to keep its quotes.
                if s.chars().any(|c| c.is_ascii_alphabetic()) {
                    out.push('\'');
                    out.push_str(&s.replace('\'', "''"));
                    out.push('\'');
                } else {
                    out.push_str(&s);
                }
            }
            Item::Field(c, n) => {
                let k = class_of(c);
                match k.and_then(|k| want.iter().find(|(wk, ..)| *wk == k).map(|w| (k, w))) {
                    Some((k, (_, wc, wn))) => {
                        // A NUMERIC field keeps the locale's own width as a
                        // FLOOR: CLDR writes `en`'s minute+second skeleton as
                        // `mm:ss`, so `{minute:"numeric", second:"numeric"}` is
                        // "02:03" and not "2:3" (`format/fractionalSecondDigits.js`),
                        // and the 24-hour `HH:mm` keeps its padded hour at
                        // midnight ("00:30:45", `PlainTime/…/resolved-time-zone.js`).
                        // `2-digit` still raises a 1-wide field.
                        // A TEXT field takes the requested width outright — that
                        // IS the width option ("May" vs "May 1" vs "M").
                        let numeric = match k {
                            0 | 6 | 7 | 12 => false, // era, weekday, dayPeriod, zone
                            3 => n <= 2 && *wn <= 2, // month has both forms
                            _ => true,
                        };
                        // The letter always comes from the request: it carries
                        // the hour CYCLE (h/H/K/k) and the zone STYLE (z/v/O).
                        let count = if numeric { (*wn).max(n) } else { *wn };
                        out.push_str(&wc.to_string().repeat(count))
                    }
                    None => out.push_str(&c.to_string().repeat(n)),
                }
            }
        }
    }
    out
}

/// Distance between a candidate `availableFormats` skeleton and the request,
/// over one half (date or time). Lower is better; `None` means the candidate
/// carries a field the request did not ask for, which disqualifies it.
fn distance(cand: &str, want: &[(u8, char, usize)]) -> Option<u32> {
    // Weights, coarsest first: a field the candidate does not carry at all
    // outweighs any number of numeric/text mismatches, which in turn outweigh
    // any number of plain width differences.
    const MISSING: u32 = 10_000;
    const WRONG_KIND: u32 = 100;
    let cf = pattern_fields(cand);
    let mut score = 0u32;
    for (k, _, n) in &cf {
        match want.iter().find(|(wk, ..)| wk == k) {
            Some((_, _, wn)) => {
                // Month is the one ECMA-402 field with both a NUMERIC form
                // (M, MM) and a TEXT one (MMM+), and UTS #35 weighs crossing
                // that boundary far above a width change. Without the weight,
                // `{year:"2-digit", month:"2-digit", day:"2-digit"}` ties
                // `yMMMd` with `yMd` and prints "05 01, 86" for "05/01/86".
                if *k == 3 && (*n >= 3) != (*wn >= 3) {
                    score += WRONG_KIND;
                }
                score += (*wn as i64 - *n as i64).unsigned_abs() as u32;
            }
            None => return None,
        }
    }
    for (k, ..) in want {
        if !cf.iter().any(|(ck, ..)| ck == k) {
            score += MISSING; // a requested field the candidate lacks
        }
    }
    Some(score)
}

/// The best CLDR pattern for one half of a request (all date fields, or all
/// time fields).
fn best_half(want: &[(u8, char, usize)], hour12: bool) -> Option<String> {
    if want.is_empty() {
        return None;
    }
    let mut best: Option<(u32, &str)> = None;
    for (sk, pat) in d::AVAILABLE_FORMATS {
        // `availableFormats` carries an `h…`/`H…` twin of every time pattern;
        // only the one matching the resolved hour cycle is a candidate.
        let f = pattern_fields(sk);
        if f.iter().any(|(k, c, _)| *k == 8 && (*c == 'h') != hour12) {
            continue;
        }
        // Plural-keyed rows ('week' W 'of' MMMM) are not skeletons.
        if sk.contains("count") {
            continue;
        }
        if let Some(dist) = distance(sk, want) {
            if best.is_none_or(|(b, _)| dist < b) {
                best = Some((dist, pat));
            }
        }
    }
    // No candidate at all (a `dayPeriod`- or `timeZoneName`-only request: CLDR
    // has no skeleton that small). The request IS then the pattern — the fields
    // in canonical order, which is what `appendItems` would build up to anyway.
    let Some((dist, pat)) = best else {
        let mut w = want.to_vec();
        w.sort_by_key(|(k, ..)| *k);
        let s: Vec<String> = w.iter().map(|(_, c, n)| c.to_string().repeat(*n)).collect();
        return Some(s.join(" "));
    };
    let mut out = pat.to_string();
    // Fields the winner lacks are attached with CLDR's `appendItems` glue, in
    // field order. `en` appends era/weekday/zone plainly and names the rest —
    // `{year, day}` is "1886 (day: 1)", which no pattern in the table spells.
    if dist >= 10_000 {
        let mut missing: Vec<&(u8, char, usize)> = want
            .iter()
            .filter(|(k, ..)| !pattern_fields(&out).iter().any(|(pk, ..)| pk == k))
            .collect();
        missing.sort_by_key(|(k, ..)| *k);
        for (k, c, n) in missing {
            let Some((_, glue, name)) = d::APPEND_ITEMS
                .iter()
                .find(|(l, ..)| l.chars().next().and_then(class_of) == Some(*k))
            else {
                continue;
            };
            // `{2}` is prose, not fields — quote it or "day" would parse as
            // d/a/y and render a date.
            out = glue
                .replace("{0}", &out)
                .replace("{1}", &c.to_string().repeat(*n))
                .replace("{2}", &format!("'{}'", name.replace('\'', "''")));
        }
    }
    Some(out)
}

/// `best_pattern` without the join: the date pattern, the time pattern, and the
/// index of the glue that would join them. `formatRange` needs the halves apart.
pub(crate) fn best_pattern_halves(req: &Request, hour12: bool) -> (String, String, usize) {
    let all: Vec<(u8, char, usize)> = req
        .fields
        .iter()
        .filter_map(|(c, n)| class_of(*c).map(|k| (k, *c, *n)))
        .collect();
    let date: Vec<_> = all
        .iter()
        .copied()
        .filter(|(k, ..)| is_date_class(*k))
        .collect();
    let time: Vec<_> = all
        .iter()
        .copied()
        .filter(|(k, ..)| !is_date_class(*k) && *k != 11)
        .collect();
    let frac = all.iter().find(|(k, ..)| *k == 11).copied();
    let dp = best_half(&date, hour12)
        .map(|p| adjust_widths(&p, req))
        .unwrap_or_default();
    let tp = best_half(&time, hour12)
        .map(|p| adjust_widths(&p, req))
        .map(|p| match frac {
            Some((_, c, n)) => splice_fraction(&p, c, n),
            None => p,
        })
        .unwrap_or_default();
    let glue = glue_index(&dp);
    (dp, tp, glue)
}

/// Attach `SSS…` to the seconds field with the locale's decimal separator, so
/// `{second, fractionalSecondDigits: 2}` reads "47.00" and not "47 00".
fn splice_fraction(pattern: &str, c: char, n: usize) -> String {
    let mut out = String::new();
    let mut done = false;
    for item in parse_pattern(pattern) {
        match item {
            Item::Lit(s) => {
                if s.chars().any(|c| c.is_ascii_alphabetic()) {
                    out.push('\'');
                    out.push_str(&s.replace('\'', "''"));
                    out.push('\'');
                } else {
                    out.push_str(&s);
                }
            }
            Item::Field(fc, fn_) => {
                out.push_str(&fc.to_string().repeat(fn_));
                if fc == 's' && !done {
                    done = true;
                    out.push_str(d::SYM_DECIMAL);
                    out.push_str(&c.to_string().repeat(n));
                }
            }
        }
    }
    if !done {
        out.push_str(&c.to_string().repeat(n));
    }
    out
}

/// The same join, on already-parsed items: `{1}` takes the date half and `{0}`
/// the time half. Splicing items rather than pattern text keeps the halves'
/// quoting intact (a re-serialised `'at'` would otherwise be re-parsed).
pub(crate) fn splice_glue(glue: &str, date: &[Item], time: &[Item]) -> Vec<Item> {
    let mut out: Vec<Item> = vec![];
    let mut buf = String::new();
    let mut chars = glue.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            let idx = chars.peek().copied();
            if matches!(idx, Some('0') | Some('1')) {
                chars.next();
                if chars.peek() == Some(&'}') {
                    chars.next();
                    if !buf.is_empty() {
                        out.push(Item::Lit(std::mem::take(&mut buf)));
                    }
                    out.extend(if idx == Some('1') { date } else { time }.iter().cloned());
                    continue;
                }
                buf.push(c);
                buf.push(idx.unwrap());
                continue;
            }
        }
        // The glue's own literal text is quoted CLDR ('at'), so strip the quotes.
        if c == '\'' {
            if chars.peek() == Some(&'\'') {
                chars.next();
                buf.push('\'');
                continue;
            }
            for q in chars.by_ref() {
                if q == '\'' {
                    break;
                }
                buf.push(q);
            }
            continue;
        }
        buf.push(c);
    }
    if !buf.is_empty() {
        out.push(Item::Lit(buf));
    }
    out
}

/// The `dateStyle`/`timeStyle` patterns, which bypass skeleton matching: CLDR
/// stores those four date and four time patterns directly.
pub(crate) fn style_index(s: &str) -> Option<usize> {
    ["full", "long", "medium", "short"]
        .iter()
        .position(|x| *x == s)
}

/// The `intervalFormats` pattern for `skeleton` at the greatest differing field,
/// or None when CLDR has no row (the caller then formats both endpoints whole
/// and joins them with `INTERVAL_FALLBACK`).
pub(crate) fn interval_pattern(half: &[Item], hour12: bool, greatest: char) -> Option<String> {
    // The request is the HALF's own pattern, not the whole formatter's: a
    // date+time formatter whose time moved matches CLDR's `hms` interval rows,
    // and a `dateStyle` formatter matches the style pattern's own skeleton
    // (`M/d/yy` → `M/d/yy – M/d/yy`, not the components' `M/d/y`).
    let want: Vec<(u8, char, usize)> = half
        .iter()
        .filter_map(|i| match i {
            // The AM/PM field is never part of a skeleton — CLDR's `hms` row
            // means "h:mm:ss a" — so counting it would disqualify every
            // candidate. (`B`, the flexible day period, IS a skeleton field.)
            Item::Field(c, n) if *c != 'a' && *c != 'b' => class_of(*c).map(|k| (k, *c, *n)),
            _ => None,
        })
        .collect();
    let req = Request {
        fields: want.iter().map(|(_, c, n)| (*c, *n)).collect(),
    };
    let req = &req;
    let gk = class_of(greatest)?;
    let mut best: Option<(u32, &str)> = None;
    for (sk, gd, pat) in d::INTERVAL_FORMATS {
        if class_of(gd.chars().next()?) != Some(gk) {
            continue;
        }
        let f = pattern_fields(sk);
        if f.iter().any(|(k, c, _)| *k == 8 && (*c == 'h') != hour12) {
            continue;
        }
        if let Some(dist) = distance(sk, &want) {
            if best.is_none_or(|(b, _)| dist < b) {
                best = Some((dist, pat));
            }
        }
    }
    // An EXACT field match collapses the fields the endpoints share, which is
    // the whole point of the table ("MMM d – d, y").
    if let Some((dist, pat)) = best.filter(|(dist, _)| *dist < 10_000) {
        let _ = dist;
        return Some(adjust_widths(pat, req));
    }
    // No row carries every requested field — `en` has `hm` but no `hms`, so a
    // seconds-bearing time range has nothing to collapse against. Take the
    // locale's SEPARATOR and put the requested pattern on both sides; dropping
    // a field instead would silently change what the range means.
    let sep = best
        .and_then(|(_, pat)| {
            let items = parse_pattern(pat);
            interval_layout(&items).map(|(s, ..)| match &items[s] {
                Item::Lit(l) => l.clone(),
                _ => String::new(),
            })
        })
        .or_else(|| {
            let (_, post) = d::INTERVAL_FALLBACK.split_once("{0}")?;
            post.split_once("{1}").map(|(s, _)| s.to_string())
        })
        .unwrap_or_else(|| " \u{2013} ".to_string());
    let body = serialize(half);
    Some(format!("{body}{sep}{body}"))
}

/// Re-serialise parsed items as a CLDR pattern, re-quoting any literal that
/// would otherwise parse back as fields.
fn serialize(items: &[Item]) -> String {
    let mut out = String::new();
    for it in items {
        match it {
            Item::Lit(s) if s.chars().any(|c| c.is_ascii_alphabetic()) => {
                out.push('\'');
                out.push_str(&s.replace('\'', "''"));
                out.push('\'');
            }
            Item::Lit(s) => out.push_str(s),
            Item::Field(c, n) => out.push_str(&c.to_string().repeat(*n)),
        }
    }
    out
}

/// The flexible day period (UTS #35 §4.7) covering `minutes` past local
/// midnight, e.g. "morning1" or "noon" for `en`. `_at` rules are instants and
/// take precedence over the ranges that contain them.
pub(crate) fn day_period_key(minutes: i32) -> &'static str {
    // `midnight` is in the rule set but is NOT a flexible day period: UTS #35
    // gives it to the `b` field (am/pm/noon/midnight), while `B` — the only
    // field ECMA-402's `dayPeriod` option can produce — uses the ranges around
    // it. test262 states the same thing from the other side:
    // `format/dayPeriod-short-en.js` enumerates all 24 hours and asserts the
    // only values `en` may return are morning/noon/afternoon/evening/night.
    let usable = |k: &str| k != "midnight";
    for (key, at, ..) in d::DAY_PERIOD_RULES {
        if *at >= 0 && *at == minutes && usable(key) {
            return key;
        }
    }
    for (key, at, from, before) in d::DAY_PERIOD_RULES {
        if *at >= 0 || !usable(key) {
            continue;
        }
        // A rule may wrap past midnight (from 21:00 before 24:00 is written
        // with before = 1440, but "from 22:00 before 06:00" would not be).
        let hit = if from <= before {
            minutes >= *from && minutes < *before
        } else {
            minutes >= *from || minutes < *before
        };
        if hit {
            return key;
        }
    }
    if minutes < 720 {
        "am"
    } else {
        "pm"
    }
}

/// A day-period name at a CLDR width index (0 = wide, 1 = abbreviated,
/// 2 = narrow).
pub(crate) fn day_period_name(key: &str, width: usize) -> &'static str {
    for (k, wide, abbr, narrow) in d::DAY_PERIODS {
        if *k == key {
            return [wide, abbr, narrow][width.min(2)];
        }
    }
    if key == "pm" {
        "PM"
    } else {
        "AM"
    }
}
