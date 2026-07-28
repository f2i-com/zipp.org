#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

pub(crate) fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}
pub(crate) fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(y) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}
/// Days since 1970-01-01 for an ISO date (Howard Hinnant's days_from_civil).
pub(crate) fn iso_to_epoch_days(y: i64, m: i64, d: i64) -> i64 {
    let yy = if m <= 2 { y - 1 } else { y };
    let era = (if yy >= 0 { yy } else { yy - 399 }) / 400;
    let yoe = yy - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
pub(crate) fn epoch_days_to_iso(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
/// ISO-8601 week-of-year (weeks belong to the year holding their Thursday).
pub(crate) fn iso_week_of_year(y: i64, m: i64, d: i64) -> i64 {
    let doy = iso_to_epoch_days(y, m, d) - iso_to_epoch_days(y, 1, 1) + 1;
    let dow = iso_day_of_week(y, m, d);
    let week = (doy - dow + 10) / 7;
    if week < 1 {
        return iso_week_of_year(y - 1, 12, 31);
    }
    if week == 53 {
        let jan1 = iso_day_of_week(y, 1, 1);
        let has53 = jan1 == 4 || (is_leap_year(y) && jan1 == 3);
        if !has53 {
            return 1;
        }
    }
    week
}

/// ISO-8601 week-numbering year — the calendar year that owns the week
/// `iso_week_of_year` reports. It is `y - 1` for early-January dates that
/// belong to the previous year's last week, `y + 1` for late-December dates
/// pulled into next year's week 1, and `y` otherwise. Branches mirror
/// `iso_week_of_year` exactly so the pair stays consistent.
pub(crate) fn iso_year_of_week(y: i64, m: i64, d: i64) -> i64 {
    let doy = iso_to_epoch_days(y, m, d) - iso_to_epoch_days(y, 1, 1) + 1;
    let dow = iso_day_of_week(y, m, d);
    let week = (doy - dow + 10) / 7;
    if week < 1 {
        return y - 1;
    }
    if week == 53 {
        let jan1 = iso_day_of_week(y, 1, 1);
        let has53 = jan1 == 4 || (is_leap_year(y) && jan1 == 3);
        if !has53 {
            return y + 1;
        }
    }
    y
}

/// Nanoseconds-since-midnight for a [h,mi,s,ms,us,ns] time.
pub(crate) fn time_to_ns(f: &[i64; 6]) -> i128 {
    (f[0] as i128) * 3_600_000_000_000
        + (f[1] as i128) * 60_000_000_000
        + (f[2] as i128) * 1_000_000_000
        + (f[3] as i128) * 1_000_000
        + (f[4] as i128) * 1_000
        + (f[5] as i128)
}
/// Decompose nanoseconds-since-midnight into [h,mi,s,ms,us,ns].
pub(crate) fn ns_to_time(mut ns: i128) -> [i64; 6] {
    let h = (ns / 3_600_000_000_000) as i64;
    ns %= 3_600_000_000_000;
    let mi = (ns / 60_000_000_000) as i64;
    ns %= 60_000_000_000;
    let s = (ns / 1_000_000_000) as i64;
    ns %= 1_000_000_000;
    let ms = (ns / 1_000_000) as i64;
    ns %= 1_000_000;
    let us = (ns / 1_000) as i64;
    let nss = (ns % 1_000) as i64;
    [h, mi, s, ms, us, nss]
}
/// "HH:MM:SS" with a trimmed fractional-seconds part when sub-second fields exist.
pub(crate) fn time_string(f: &[i64; 6]) -> String {
    let sub = f[3] * 1_000_000 + f[4] * 1_000 + f[5];
    let base = format!("{:02}:{:02}:{:02}", f[0], f[1], f[2]);
    if sub == 0 {
        base
    } else {
        let frac = format!("{sub:09}");
        format!("{base}.{}", frac.trim_end_matches('0'))
    }
}
/// Parse "HH:MM[:SS[.fff]]" (separators optional) → [h,mi,s,ms,us,ns].
pub(crate) fn parse_iso_time(s: &str) -> Option<[i64; 6]> {
    let s = s.trim();
    // Allow a leading "T".
    let s = s.strip_prefix(['T', 't']).unwrap_or(s);
    let b: Vec<char> = s.chars().collect();
    let n = b.len();
    let take2 = |i: usize| -> Option<i64> {
        if i + 1 < n && b[i].is_ascii_digit() && b[i + 1].is_ascii_digit() {
            Some((b[i] as i64 - '0' as i64) * 10 + (b[i + 1] as i64 - '0' as i64))
        } else {
            None
        }
    };
    let h = take2(0)?;
    let mut i = 2;
    let mut mi = 0i64;
    let mut sec = 0i64;
    let mut sub = [0i64; 3];
    // Minutes: optional, after an optional ':' separator. A ':' must be followed
    // by two digits.
    let mut had_min = false;
    let mut min_colon = false;
    {
        let mut j = i;
        let colon = b.get(j) == Some(&':');
        if colon {
            j += 1;
        }
        if let Some(v) = take2(j) {
            mi = v;
            i = j + 2;
            had_min = true;
            min_colon = colon;
        } else if colon {
            return None;
        }
    }
    // Seconds (only meaningful when minutes were present), then an optional
    // fractional part — fractions are ONLY allowed after seconds, never after a
    // bare hour or minute (those make the string invalid).
    let mut had_sec = false;
    let mut sec_colon = false;
    if had_min {
        let mut j = i;
        let colon = b.get(j) == Some(&':');
        if colon {
            j += 1;
        }
        if let Some(v) = take2(j) {
            sec = v;
            i = j + 2;
            had_sec = true;
            sec_colon = colon;
            if b.get(i) == Some(&'.') || b.get(i) == Some(&',') {
                i += 1;
                let start = i;
                while b.get(i).is_some_and(|c| c.is_ascii_digit()) {
                    i += 1;
                }
                let cnt = i - start;
                // 1..=9 fractional digits; more than 9 is a RangeError.
                if cnt == 0 || cnt > 9 {
                    return None;
                }
                let mut fr: String = b[start..i].iter().collect();
                while fr.len() < 9 {
                    fr.push('0');
                }
                let ns: i64 = fr.parse().ok()?;
                sub = [ns / 1_000_000, (ns / 1_000) % 1_000, ns % 1_000];
            }
        } else if colon {
            return None;
        }
    }
    // The hour:minute and minute:second separators must be consistent — both ':'
    // (extended) or both absent (basic). Reject "0000:00" / "00:0000".
    if had_sec && min_colon != sec_colon {
        return None;
    }
    // The remaining suffix must be empty, OR a fully well-formed designator —
    // "Z"/"z" or a valid UTC offset — optionally followed by a "[...]" annotation
    // block (validated upstream). A stray '.'/',' or trailing junk is invalid.
    if i < n {
        let suffix: String = b[i..].iter().collect();
        let head = match suffix.find('[') {
            Some(p) => &suffix[..p],
            None => suffix.as_str(),
        };
        if !(head.is_empty()
            || head.eq_ignore_ascii_case("z")
            || super::temporal::valid_offset_string(head))
        {
            return None;
        }
    }
    // A leap second (:60) is accepted and clamped to :59 (per Temporal parsing).
    if sec == 60 {
        sec = 59;
    }
    if !(0..24).contains(&h) || !(0..60).contains(&mi) || !(0..60).contains(&sec) {
        return None;
    }
    Some([h, mi, sec, sub[0], sub[1], sub[2]])
}

/// True if a bare time-candidate (annotations already stripped, no time
/// designator) is ALSO a valid ISO date / year-month / month-day form — which
/// makes it ambiguous as a Temporal time, so the grammar then requires an
/// explicit `T`. Rejects e.g. "2021-12"/"1214"/"202112"/"12-14" (valid date
/// forms) while letting "2021-13"/"1314"/"0000" (invalid month/day → a time)
/// through. Mirrors the spec's disambiguation of CalendarTime vs DateSpec.
pub(crate) fn ambiguous_with_date(s: &str) -> bool {
    if parse_iso_date(s).is_some() {
        return true; // a full date (incl. basic YYYYMMDD) is unambiguously a date
    }
    let b = s.as_bytes();
    let alld = |x: &[u8]| !x.is_empty() && x.iter().all(u8::is_ascii_digit);
    let two = |x: &[u8]| (x[0] - b'0') as i64 * 10 + (x[1] - b'0') as i64;
    let vm = |m: i64| (1..=12).contains(&m);
    let vmd = |m: i64, d: i64| vm(m) && d >= 1 && d <= days_in_month(1972, m);
    match b.len() {
        4 if alld(b) => vmd(two(&b[0..2]), two(&b[2..4])), // MMDD
        5 if b[2] == b'-' && alld(&b[0..2]) && alld(&b[3..5]) => vmd(two(&b[0..2]), two(&b[3..5])), // MM-DD
        6 if alld(b) => vm(two(&b[4..6])),                 // YYYYMM
        7 if b[4] == b'-' && alld(&b[0..4]) && alld(&b[5..7]) => vm(two(&b[5..7])), // YYYY-MM
        _ => false,
    }
}

/// ParseTemporalTimeString: a bare time, OR a full date-time whose time part is
/// extracted, optionally followed by a UTC-offset and/or annotation suffix.
/// `None` if invalid, or if a bare date-like string is given with no time
/// designator (ambiguous — the grammar requires a `T`). A date-only string
/// (e.g. "2021-08-19") is therefore rejected (no implicit midnight).
pub(crate) fn parse_temporal_time(s: &str) -> Option<[i64; 6]> {
    let s = s.trim();
    let main = match s.find('[') {
        Some(i) => s[..i].trim_end(),
        None => s,
    };
    // Explicit time designator "T<time>".
    if let Some(rest) = main.strip_prefix(['T', 't']) {
        return parse_iso_time(rest);
    }
    // "<date>T<time>" (or space-separated): the date must be valid; take the time.
    if let Some(ti) = main.find(['T', 't']) {
        parse_iso_date(&main[..ti])?;
        return parse_iso_time(&main[ti + 1..]);
    }
    if let Some(si) = main.find(' ') {
        if parse_iso_date(&main[..si]).is_some() {
            return parse_iso_time(&main[si + 1..]);
        }
    }
    // Bare string, no designator: reject if it is a valid date form (ambiguous).
    if ambiguous_with_date(main) {
        return None;
    }
    // A bare DATE carrying a UTC designator / numeric offset but NO time
    // ("2022-09-15Z", "2022-09-15+00:00", "2022-09-15-02:30") is not a valid
    // PlainTime string — the date "YYYY-MM-DD" is followed by Z/±offset, not a time.
    if main.len() >= 10 {
        let (date_candidate, rest) = main.split_at(10);
        if parse_iso_date(date_candidate).is_some()
            && matches!(rest.chars().next(), Some('Z' | 'z' | '+' | '-'))
        {
            return None;
        }
    }
    parse_iso_time(main)
}

/// Parse "YYYY-MM-DD[THH:MM:SS.fff]" → [y,mo,d,h,mi,s,ms,us,ns] (time defaults 0).
pub(crate) fn parse_iso_datetime(s: &str) -> Option<[i64; 9]> {
    let s = s.trim();
    let (date_s, time_s) = match s.find(['T', 't']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => match s.find(' ') {
            Some(i) => (&s[..i], Some(&s[i + 1..])),
            None => (s, None),
        },
    };
    let (y, mo, d) = parse_iso_date(date_s)?;
    let t = match time_s {
        Some(ts) if !ts.is_empty() => parse_iso_time(ts)?,
        // A present `T`/space separator requires a valid time (reject "2020-01-01T").
        Some(_) => return None,
        None => [0; 6],
    };
    Some([y, mo, d, t[0], t[1], t[2], t[3], t[4], t[5]])
}

/// Nanoseconds in a day.
pub(crate) const DAY_NS: i128 = 86_400_000_000_000;

/// Epoch-nanoseconds → "YYYY-MM-DDTHH:MM:SSZ" (UTC).
pub(crate) fn instant_to_string(ns: i128) -> String {
    let days = ns.div_euclid(DAY_NS) as i64;
    let rem = ns.rem_euclid(DAY_NS);
    let (y, m, d) = epoch_days_to_iso(days);
    let t = ns_to_time(rem);
    // Instant.toString always shows whole seconds (sub-second only if present).
    let base = format!("{:02}:{:02}:{:02}", t[0], t[1], t[2]);
    let sub = t[3] * 1_000_000 + t[4] * 1_000 + t[5];
    let time = if sub == 0 {
        base
    } else {
        let frac = format!("{sub:09}");
        format!("{base}.{}", frac.trim_end_matches('0'))
    };
    format!("{}T{}Z", iso_date_string(y, m, d), time)
}

/// Parse "+HH:MM"/"-HH:MM"/"+HHMM"/"Z" UTC offset → nanoseconds (Z → 0).
pub(crate) fn parse_offset_ns(s: &str) -> Option<i128> {
    if matches!(s, "Z" | "z") {
        return Some(0);
    }
    let sign: i128 = match s.as_bytes().first() {
        Some(b'+') => 1,
        Some(b'-') => -1,
        _ => return None,
    };
    // Split the integer part (±HH[:]MM[:]SS) from an optional sub-second fraction.
    let body = &s[1..];
    let (int_part, frac) = match body.split_once(['.', ',']) {
        Some((a, b)) => (a, b),
        None => (body, ""),
    };
    let digits: String = int_part.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 2 {
        return None;
    }
    let h: i128 = digits[..2].parse().ok()?;
    let mi: i128 = if digits.len() >= 4 { digits[2..4].parse().ok()? } else { 0 };
    let sec: i128 = if digits.len() >= 6 { digits[4..6].parse().ok()? } else { 0 };
    let frac_digits: String = frac.chars().take_while(|c| c.is_ascii_digit()).collect();
    if frac_digits.len() > 9 {
        return None;
    }
    let frac_ns: i128 = if frac_digits.is_empty() {
        0
    } else {
        frac_digits.parse::<i128>().ok()? * 10i128.pow(9 - frac_digits.len() as u32)
    };
    Some(sign * (h * 3_600_000_000_000 + mi * 60_000_000_000 + sec * 1_000_000_000 + frac_ns))
}

/// Parse an ISO instant string ("…Z" or "…±HH:MM") → epoch nanoseconds (UTC).
pub(crate) fn instant_str_to_ns(s: &str) -> Option<i128> {
    let s = s.trim();
    // Drop a trailing [...] annotation block (its validity is enforced upstream by
    // temporal_string_ok); the remaining "main" must be DateTime + (Z | numeric UTC
    // offset) with NOTHING after the designator.
    let main = match s.find('[') {
        Some(i) => &s[..i],
        None => s,
    };
    // DateTimeSeparator is `T` | `t` | <space>.
    let tpos = main.find(['T', 't', ' '])?;
    let after_t = &main[tpos + 1..];
    // The designator runs from the first Z/+/- to the end of `main` (so trailing junk
    // is part of `off` and rejected below). An Instant string must carry a designator.
    let off = if let Some(z) = after_t.find(['Z', 'z']) {
        &after_t[z..]
    } else if let Some(rel) = after_t.find(['+', '-']) {
        &after_t[rel..]
    } else {
        return None;
    };
    // The time portion between the separator and the designator must be non-empty
    // (rejects "…T" with no time), and the designator must be exactly "Z"/"z" or a
    // well-formed UTC offset with no trailing characters.
    let time_part = &after_t[..after_t.len() - off.len()];
    if time_part.is_empty() {
        return None;
    }
    let off_ns = if off == "Z" || off == "z" {
        0
    } else if super::temporal::valid_offset_string(off) {
        parse_offset_ns(off)?
    } else {
        return None;
    };
    let dt_part = &main[..tpos + 1 + time_part.len()];
    let dt = parse_iso_datetime(dt_part)?;
    let ns = (iso_to_epoch_days(dt[0], dt[1], dt[2]) as i128) * DAY_NS
        + time_to_ns(&[dt[3], dt[4], dt[5], dt[6], dt[7], dt[8]])
        - off_ns;
    Some(ns)
}

/// "YYYY-MM-DD" (expanded ±YYYYYY for years outside 0..9999).
pub(crate) fn iso_date_string(y: i64, m: i64, d: i64) -> String {
    let ys = if (0..=9999).contains(&y) {
        format!("{y:04}")
    } else {
        format!("{y:+07}")
    };
    format!("{ys}-{m:02}-{d:02}")
}

/// "YYYY-MM" (expanded-year aware) — Temporal.PlainYearMonth serialization.
pub(crate) fn year_month_string(y: i64, m: i64) -> String {
    let ys = if (0..=9999).contains(&y) {
        format!("{y:04}")
    } else {
        format!("{y:+07}")
    };
    format!("{ys}-{m:02}")
}

/// Parse a month code like "M06" (ISO calendars have no leap months) → 1..=12.
/// Whether `(y, m, d)` is a representable `Temporal.PlainDate` — within the
/// day-granular range `[-271821-04-19, +275760-09-13]`. (ISODateWithinLimits checks
/// the date at noon, so the bound is at day granularity; `PlainDateTime`/`Instant`
/// use a finer, nanosecond-precise epoch bound instead.)
pub(crate) fn iso_date_in_range(y: i64, m: i64, d: i64) -> bool {
    (y, m, d) >= (-271821, 4, 19) && (y, m, d) <= (275760, 9, 13)
}

/// Whether `(y, m)` is a representable `Temporal.PlainYearMonth` — within the
/// year-month-granular range `[-271821-04, +275760-09]` (ISOYearMonthWithinLimits);
/// any day/time inside the boundary month is in range.
pub(crate) fn iso_year_month_in_range(y: i64, m: i64) -> bool {
    (y, m) >= (-271821, 4) && (y, m) <= (275760, 9)
}

pub(crate) fn parse_month_code(s: &str) -> Option<i64> {
    // MonthCode grammar: "M" followed by EXACTLY two ASCII digits (so "M1"/"M005"
    // are malformed). A trailing "L" marks a lunisolar leap month, which the ISO
    // 8601 calendar does not have — reject it. The two-digit value must be 1..=12.
    match parse_month_code_syntax(s) {
        Some((n, false)) if (1..=12).contains(&n) => Some(n),
        _ => None,
    }
}

/// Parse a monthCode for *syntax* only, separating well-formedness from
/// ISO-calendar validity. Returns `Some((month, is_leap))` for any well-formed
/// code ("M" + exactly two ASCII digits + optional trailing "L"), or `None` if
/// the string is malformed. The caller decides whether the (month, leap) pair is
/// valid for its calendar — for ISO that means `1..=12` and `!is_leap`. This
/// split lets `with()` defer a calendar-invalid-but-well-formed code (e.g.
/// "M08L", "M13") past the point where the options bag is read, per spec.
pub(crate) fn parse_month_code_syntax(s: &str) -> Option<(i64, bool)> {
    let body = s.strip_prefix('M')?;
    let (digits, is_leap) = match body.strip_suffix('L') {
        Some(d) => (d, true),
        None => (body, false),
    };
    if digits.len() != 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = digits.parse::<i64>().ok()?;
    Some((n, is_leap))
}

/// Parse "YYYY-MM" (or a fuller ISO date) → (year, month, referenceISODay).
pub(crate) fn parse_iso_year_month(s: &str) -> Option<(i64, i64, i64)> {
    let s = s.trim();
    if let Some((y, m, d)) = parse_iso_date(s) {
        return Some((y, m, d));
    }
    let bytes = s.as_bytes();
    let (sign, rest) = match bytes.first() {
        Some(b'-') | Some(b'+') => (if bytes[0] == b'-' { -1i64 } else { 1 }, &s[1..]),
        _ => (1, s),
    };
    // An expanded 6-digit year requires the ± sign; basic "YYYYMM" is 4-digit year.
    let signed = matches!(bytes.first(), Some(b'-') | Some(b'+'));
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let ylen = if signed && digits.len() >= 6 { 6 } else { 4 };
    if digits.len() < ylen {
        return None;
    }
    let yv = rest[..ylen].parse::<i64>().ok()?;
    if sign < 0 && yv == 0 {
        return None;
    }
    let y = sign * yv;
    let after = &rest[ylen..];
    let after = after.strip_prefix('-').unwrap_or(after);
    if after.len() < 2 {
        return None;
    }
    let m = after[..2].parse::<i64>().ok()?;
    if !(1..=12).contains(&m) {
        return None;
    }
    // Only a bare year-month reaches this fallback (a fuller date/time form is caught
    // by parse_iso_date earlier); anything after MM other than a "[…]" annotation
    // (e.g. an offset, fractional time, or trailing junk) makes the string invalid.
    let rem = &after[2..];
    if !rem.is_empty() && !rem.starts_with('[') {
        return None;
    }
    Some((y, m, 1))
}

/// Parse "MM-DD" / "--MM-DD" (or a fuller ISO date) → (referenceISOYear, month, day).
pub(crate) fn parse_iso_month_day(s: &str) -> Option<(i64, i64, i64)> {
    let s = s.trim();
    // A full date string yields its month/day with the ISO reference year 1972
    // (a leap year, so "02-29" stays valid), NOT the string's own year.
    if let Some((_, m, d)) = parse_iso_date(s) {
        return Some((1972, m, d));
    }
    let body = s.strip_prefix("--").unwrap_or(s);
    if body.len() < 4 {
        return None;
    }
    let m = body.get(..2)?.parse::<i64>().ok()?;
    let after = &body[2..];
    let after = after.strip_prefix('-').unwrap_or(after);
    if after.len() < 2 {
        return None;
    }
    let d = after[..2].parse::<i64>().ok()?;
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(1972, m) {
        return None;
    }
    // Only a bare month-day reaches this fallback; anything after DD other than a
    // "[…]" annotation makes the string invalid (offset / fractional / junk).
    let rem = &after[2..];
    if !rem.is_empty() && !rem.starts_with('[') {
        return None;
    }
    Some((1972, m, d))
}

/// Canonicalize a BCP-47 language tag. Structure and ordering only — the CLDR
/// alias tables (`iw`→`he`, `art-lojban`→`jbo`, …) are locale data this engine
/// does not ship, so an aliased tag round-trips unchanged. See `locale_tag.rs`
/// for the grammar. Returns None if the tag is not structurally valid.
pub(crate) fn canonicalize_locale(tag: &str) -> Option<String> {
    crate::vm::locale_tag::canonicalize_tag(tag)
}

/// The resolved NumberFormat slots that drive FormatNumeric. Passed as a struct
/// because ECMA-402's rounding depends on all of them together (a significant-
/// digit target, a fraction-digit target, or both under morePrecision).
pub(crate) struct NumFmtParams<'a> {
    pub style: &'a str,
    /// "standard" | "scientific" | "engineering" | "compact". `compact` needs
    /// the CLDR compact-decimal patterns, which this engine does not ship, so it
    /// formats as `standard`.
    pub notation: &'a str,
    pub min_int: i64,
    pub min_frac: Option<i64>,
    pub max_frac: Option<i64>,
    pub min_sig: Option<i64>,
    pub max_sig: Option<i64>,
    pub rounding_priority: &'a str,
    pub rounding_mode: &'a str,
    pub rounding_increment: i64,
    pub trailing_zero_display: &'a str,
    pub sign_display: &'a str,
    pub grouping: bool,
    /// `useGrouping: "min2"` — CLDR's `minimumGroupingDigits` raised to 2, i.e.
    /// the group separators appear only once the integer part reaches
    /// `groupingSize + 2` = 5 digits (1000 → "1000", 10000 → "10,000"). It is a
    /// property of the WHOLE integer part, not of the leading group: 1000000
    /// still groups fully even though its leading group is one digit.
    pub group_min2: bool,
}

/// The decimal digits a non-negative finite f64 rounds from, as (integer,
/// fraction). This is the SHORTEST round-tripping representation (Rust's
/// `Display`), not the exact binary expansion: ECMA-402's "Intl mathematical
/// value" is the decimal the double denotes, so `1.15` must round to `1.2`
/// under halfExpand even though the stored binary value is 1.14999999999999991.
fn exact_decimal(x: f64) -> (String, String) {
    let s = format!("{x}");
    match s.split_once('.') {
        Some((i, f)) => (i.to_string(), f.to_string()),
        None => (s, String::new()),
    }
}

/// Fold a signed rounding mode into a magnitude decision: `ceil`/`floor` and
/// their half- forms depend on the sign, the rest do not.
fn fold_rounding_mode(mode: &str, neg: bool) -> &'static str {
    match mode {
        "ceil" => if neg { "trunc" } else { "expand" },
        "floor" => if neg { "expand" } else { "trunc" },
        "halfCeil" => if neg { "halfTrunc" } else { "halfExpand" },
        "halfFloor" => if neg { "halfExpand" } else { "halfTrunc" },
        "expand" => "expand",
        "trunc" => "trunc",
        "halfTrunc" => "halfTrunc",
        "halfEven" => "halfEven",
        _ => "halfExpand",
    }
}

/// Should the kept prefix be incremented? `cmp` compares the dropped remainder
/// against exactly one half of the rounding unit.
fn round_up(mode: &str, nonzero: bool, cmp: std::cmp::Ordering, last_odd: bool) -> bool {
    use std::cmp::Ordering::*;
    if !nonzero {
        return false;
    }
    match mode {
        "expand" => true,
        "trunc" => false,
        "halfTrunc" => cmp == Greater,
        "halfEven" => cmp == Greater || (cmp == Equal && last_odd),
        _ => cmp != Less, // halfExpand
    }
}

/// Add one to a decimal digit string, growing it on carry-out ("999" → "1000").
fn bump(digits: &mut Vec<u8>) {
    for d in digits.iter_mut().rev() {
        if *d < 9 {
            *d += 1;
            return;
        }
        *d = 0;
    }
    digits.insert(0, 1);
}

/// Round the non-negative decimal `int_s`.`frac_s` at fraction position `k`
/// (negative k rounds the integer part to tens/hundreds/…), returning the digit
/// string scaled to exactly `k` fraction places when k ≥ 0.
fn round_decimal_at(int_s: &str, frac_s: &str, k: i64, mode: &str) -> (String, String) {
    let all: Vec<u8> = int_s.bytes().chain(frac_s.bytes()).map(|b| b - b'0').collect();
    let point = int_s.len() as i64;
    let keep = (point + k).max(0) as usize;
    let mut kept: Vec<u8> = all.iter().copied().take(keep).collect();
    while kept.len() < keep {
        kept.push(0);
    }
    let rest = if keep < all.len() { &all[keep..] } else { &[][..] };
    let nonzero = rest.iter().any(|d| *d != 0);
    let cmp = match rest.first() {
        None => std::cmp::Ordering::Less,
        Some(&d) if d > 5 => std::cmp::Ordering::Greater,
        Some(&d) if d < 5 => std::cmp::Ordering::Less,
        _ => {
            if rest[1..].iter().any(|d| *d != 0) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        }
    };
    let last_odd = kept.last().is_some_and(|d| d % 2 == 1);
    if round_up(mode, nonzero, cmp, last_odd) {
        if kept.is_empty() {
            kept.push(1);
        } else {
            bump(&mut kept);
        }
    }
    // Re-split at the point. `bump` may have prepended a digit, and a negative k
    // means the dropped integer places come back as zeros.
    let mut s: String = kept.iter().map(|d| (d + b'0') as char).collect();
    let grown = kept.len() as i64 - keep as i64;
    if k < 0 {
        for _ in 0..(-k) {
            s.push('0');
        }
        return (if s.is_empty() { "0".into() } else { s }, String::new());
    }
    let ip = (point + grown).max(0) as usize;
    while s.len() < ip {
        s.insert(0, '0');
    }
    let (i, f) = s.split_at(ip.min(s.len()));
    (if i.is_empty() { "0".to_string() } else { i.to_string() }, f.to_string())
}

/// The decimal exponent of the most significant digit (1 for "1.5", 0 for
/// "0.5", -1 for "0.05"); 1 for an all-zero value so a zero keeps one digit.
fn decimal_exponent(int_s: &str, frac_s: &str) -> i64 {
    if int_s.bytes().any(|b| b != b'0') {
        return int_s.trim_start_matches('0').len() as i64;
    }
    match frac_s.bytes().position(|b| b != b'0') {
        Some(i) => -(i as i64),
        None => 1,
    }
}

/// Round to a multiple of `inc` at fraction position `k`. The scaled value is
/// held in i128, which covers every roundingIncrement the spec allows against a
/// realistic magnitude; anything wider falls back to plain rounding.
fn round_to_increment(int_s: &str, frac_s: &str, k: i64, inc: i64, mode: &str) -> Option<(String, String)> {
    let k = k.max(0) as usize;
    let mut scaled = String::from(int_s);
    let f: String = frac_s.chars().chain(std::iter::repeat('0')).take(k).collect();
    scaled.push_str(&f);
    let q: i128 = scaled.trim_start_matches('0').parse().unwrap_or(0);
    let rest = &frac_s[k.min(frac_s.len())..];
    let rest_nonzero = rest.bytes().any(|b| b != b'0');
    let inc = inc as i128;
    let lo = (q / inc) * inc;
    // Distance from the lower multiple, in units of 1/2 the increment, using the
    // dropped tail only to break an exact tie.
    let off = q - lo;
    let cmp = (off * 2).cmp(&inc);
    let cmp = if cmp == std::cmp::Ordering::Equal && rest_nonzero {
        std::cmp::Ordering::Greater
    } else {
        cmp
    };
    let nonzero = off != 0 || rest_nonzero;
    let last_odd = (lo / inc) % 2 == 1;
    let v = if round_up(mode, nonzero, cmp, last_odd) { lo + inc } else { lo };
    let mut s = v.to_string();
    while s.len() <= k {
        s.insert(0, '0');
    }
    let (i, fr) = s.split_at(s.len() - k);
    Some((i.to_string(), fr.to_string()))
}

/// Format a number for Intl.NumberFormat: ECMA-402 rounding (fraction digits,
/// significant digits, rounding mode + increment, trailing-zero display) and
/// sign display, then en-US decoration ("," grouping, "." decimal; percent
/// multiplies by 100).
pub(crate) fn format_number_intl(n: f64, p: &NumFmtParams) -> String {
    // The sign comes from the INPUT, so -0.0001 rounded to "0" still prints
    // "-0" under signDisplay "auto"; only exceptZero/negative consult the
    // ROUNDED magnitude (below).
    let neg = n.is_sign_negative() && !n.is_nan();
    let decorate = |body: String, neg: bool, zero: bool, nan: bool| -> String {
        let sign = match p.sign_display {
            "never" => "",
            "always" => {
                if neg { "-" } else { "+" }
            }
            "exceptZero" => {
                if zero || nan {
                    ""
                } else if neg {
                    "-"
                } else {
                    "+"
                }
            }
            "negative" => {
                if neg && !zero && !nan { "-" } else { "" }
            }
            _ => {
                if neg { "-" } else { "" }
            }
        };
        format!("{sign}{body}")
    };
    if n.is_nan() {
        return decorate("NaN".to_string(), false, false, true);
    }
    let mut x = n.abs();
    if p.style == "percent" {
        x *= 100.0;
    }
    if x.is_infinite() {
        let body = if p.style == "percent" { "∞%".to_string() } else { "∞".to_string() };
        return decorate(body, neg, false, false);
    }
    let mode = fold_rounding_mode(p.rounding_mode, neg);
    let (int_s, frac_s) = exact_decimal(x);
    // ComputeExponent (ECMA-402 15.5.11): scientific puts one digit before the
    // point, engineering rounds that exponent down to a multiple of three. The
    // mantissa is produced by SHIFTING the decimal string, not by dividing —
    // `x / 1e-6` would re-introduce binary error into an exact decimal.
    let magnitude: Option<i64> = match p.notation {
        "scientific" | "engineering" => {
            let is_zero_in = int_s.bytes().all(|b| b == b'0') && frac_s.bytes().all(|b| b == b'0');
            Some(if is_zero_in { 0 } else { decimal_exponent(&int_s, &frac_s) - 1 })
        }
        _ => None,
    };
    let exp_for = |m: i64| if p.notation == "engineering" { m.div_euclid(3) * 3 } else { m };
    let exponent = magnitude.map(exp_for);
    let (orig_int, orig_frac) = (int_s.clone(), frac_s.clone());
    let (int_s, frac_s) = match exponent {
        Some(e) => shift_decimal(&int_s, &frac_s, e),
        None => (int_s, frac_s),
    };
    // The fraction-digit and significant-digit targets are separate roundings;
    // "morePrecision"/"lessPrecision" run both and pick between them, "auto"
    // only ever has one configured.
    let by_frac = p.max_frac.map(|k| {
        if p.rounding_increment != 1 {
            round_to_increment(&int_s, &frac_s, k, p.rounding_increment, mode)
                .unwrap_or_else(|| round_decimal_at(&int_s, &frac_s, k, mode))
        } else {
            round_decimal_at(&int_s, &frac_s, k, mode)
        }
    });
    let by_sig = p.max_sig.map(|sd| {
        let k = sd - decimal_exponent(&int_s, &frac_s);
        round_decimal_at(&int_s, &frac_s, k, mode)
    });
    // Digit-count of a candidate, used to pick the more/less precise one.
    let precision = |v: &(String, String)| -> usize { v.1.trim_end_matches('0').len() };
    let (mut ip, mut fp) = match (by_frac, by_sig) {
        (Some(f), Some(s)) => {
            if p.rounding_priority == "lessPrecision" {
                if precision(&f) <= precision(&s) { f } else { s }
            } else {
                if precision(&f) >= precision(&s) { f } else { s }
            }
        }
        (Some(f), None) => f,
        (None, Some(s)) => s,
        (None, None) => round_decimal_at(&int_s, &frac_s, 3, mode),
    };
    // Pad/strip the fraction to the configured minimum. Under significant-digit
    // rounding the minimum comes from minimumSignificantDigits instead.
    let min_frac = match (p.min_frac, p.min_sig) {
        (Some(m), _) => m,
        (None, Some(sd)) => (sd - decimal_exponent(&ip, &fp)).max(0),
        (None, None) => 0,
    };
    while (fp.len() as i64) < min_frac {
        fp.push('0');
    }
    while fp.len() as i64 > min_frac && fp.ends_with('0') {
        fp.pop();
    }
    // trailingZeroDisplay "stripIfInteger": drop the whole fraction when it is
    // all zeros, overriding the minimum.
    if p.trailing_zero_display == "stripIfInteger" && fp.bytes().all(|b| b == b'0') {
        fp.clear();
    }
    while (ip.len() as i64) < p.min_int {
        ip.insert(0, '0');
    }
    // ComputeExponent steps 8-11: rounding the mantissa can carry it up a digit
    // (9.99 at one fraction digit becomes 10.0). When it does, the exponent is
    // recomputed from magnitude+1 — which for engineering may or may not move,
    // since it re-floors to a multiple of three.
    let mut exponent = exponent;
    if let (Some(e), Some(m)) = (exponent, magnitude) {
        let rounded_zero = ip.bytes().all(|b| b == b'0') && fp.bytes().all(|b| b == b'0');
        let new_mag = decimal_exponent(&ip, &fp) - 1;
        if !rounded_zero && new_mag != m - e {
            let e2 = exp_for(m + 1);
            if e2 != e {
                let (i2, f2) = shift_decimal(&orig_int, &orig_frac, e2);
                let (i3, f3) = round_decimal_at(&i2, &f2, fp.len() as i64, mode);
                ip = i3;
                fp = f3;
                exponent = Some(e2);
            }
        }
    }
    let is_zero = ip.bytes().all(|b| b == b'0') && fp.bytes().all(|b| b == b'0');
    // The scientific/engineering pattern has no grouping separators.
    let min_group_len = if p.group_min2 { 5 } else { 4 };
    let grouped = if p.grouping && exponent.is_none() && ip.len() >= min_group_len {
        let len = ip.len();
        let first = match len % 3 {
            0 => 3,
            r => r,
        };
        let mut out = String::from(&ip[..first]);
        let mut i = first;
        while i < len {
            out.push(',');
            out.push_str(&ip[i..i + 3]);
            i += 3;
        }
        out
    } else {
        ip
    };
    let mut res = grouped;
    if !fp.is_empty() {
        res.push('.');
        res.push_str(&fp);
    }
    if let Some(e) = exponent {
        // The en scientific pattern's exponent separator is "E"; the exponent
        // itself carries a minus but never a plus.
        res.push('E');
        if e < 0 {
            res.push('-');
        }
        res.push_str(&e.abs().to_string());
    }
    if p.style == "percent" {
        res.push('%');
    }
    decorate(res, neg, is_zero, false)
}

/// Move a decimal string's point `e` places to the LEFT (i.e. divide by 10^e),
/// exactly. `shift_decimal("543", "211", 3)` is `("0", "543211")`.
fn shift_decimal(int_s: &str, frac_s: &str, e: i64) -> (String, String) {
    let digits: String = format!("{int_s}{frac_s}");
    let point = int_s.len() as i64 - e;
    let (int, frac) = if point <= 0 {
        let pad = "0".repeat((-point) as usize);
        (String::from("0"), format!("{pad}{digits}"))
    } else if point as usize >= digits.len() {
        let pad = "0".repeat(point as usize - digits.len());
        (format!("{digits}{pad}"), String::new())
    } else {
        (digits[..point as usize].to_string(), digits[point as usize..].to_string())
    };
    // "0.000345" shifted by -6 is "0000345"; the mantissa must read "345"
    // (minimumIntegerDigits re-pads afterwards if it asks for more).
    let trimmed = int.trim_start_matches('0');
    let int = if trimmed.is_empty() { "0".to_string() } else { trimmed.to_string() };
    (int, frac)
}

/// A short currency symbol for the common codes (en-US "symbol" display); unknown
/// codes fall back to the code plus a non-breaking space.
pub(crate) fn currency_symbol(code: &str) -> String {
    match code {
        "USD" => "$".to_string(),
        "EUR" => "€".to_string(),
        "GBP" => "£".to_string(),
        "JPY" | "CNY" => "¥".to_string(),
        "INR" => "₹".to_string(),
        "KRW" => "₩".to_string(),
        other => format!("{other}\u{a0}"),
    }
}

/// CreatePartsFromList for the en LONG conjunction/disjunction list patterns —
/// the only pattern set this engine has. `format` joins these; `formatToParts`
/// wraps them. The short/narrow widths and the `unit` type need CLDR
/// listPatterns, so they format identically to `long` here.
pub(crate) fn list_parts_en(items: &[String], conj: &str) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = vec![];
    let n = items.len();
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            let lit = if n == 2 {
                format!(" {conj} ")
            } else if i == n - 1 {
                format!(", {conj} ")
            } else {
                ", ".to_string()
            };
            out.push(("literal", lit));
        }
        out.push(("element", it.clone()));
    }
    out
}

/// A non-negative finite number as the typed parts RelativeTimeFormat's embedded
/// NumberFormat produces for the value it interpolates: integer runs separated by
/// `group` parts, then `decimal`/`fraction` ("in 1,000 seconds" is
/// integer "1", group ",", integer "000").
pub(crate) fn grouped_decimal_parts(n: f64) -> Vec<(&'static str, String)> {
    let s = if n.fract() == 0.0 && n.abs() < 9.007e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    };
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i.to_string(), Some(f.to_string())),
        None => (s, None),
    };
    let mut out: Vec<(&'static str, String)> = vec![];
    let len = int.len();
    let first = match len % 3 {
        0 => 3.min(len),
        r => r,
    };
    let mut i = 0usize;
    while i < len {
        let take = if i == 0 { first } else { 3 };
        if i > 0 {
            out.push(("group", ",".to_string()));
        }
        out.push(("integer", int[i..i + take].to_string()));
        i += take;
    }
    if let Some(f) = frac {
        out.push(("decimal", ".".to_string()));
        out.push(("fraction", f));
    }
    out
}

/// Minimal en duration formatting: non-zero fields joined as "N unit, …".
pub(crate) fn format_duration_en(d: &[i64; 10]) -> String {
    let parts: Vec<String> = d
        .iter()
        .enumerate()
        .filter(|(_, &v)| v != 0)
        .map(|(i, &v)| format!("{v} {}", duration_unit_label(i)))
        .collect();
    if parts.is_empty() {
        "0 sec".to_string()
    } else {
        parts.join(", ")
    }
}

/// The en "short"-style label of a Duration field, by DURATION_FIELDS index —
/// shared by `format` and `formatToParts` so the two never drift apart.
pub(crate) fn duration_unit_label(i: usize) -> &'static str {
    const NAMES: [&str; 10] = ["yr", "mth", "wk", "day", "hr", "min", "sec", "ms", "μs", "ns"];
    NAMES[i.min(9)]
}

/// Normalize a Temporal unit option: strip a trailing plural "s"; "auto"→`auto_to`.
pub(crate) fn normalize_unit(u: &str, auto_to: &str) -> String {
    let base = u.strip_suffix('s').unwrap_or(u);
    if base == "auto" { auto_to.to_string() } else { base.to_string() }
}

/// DifferenceISODate: the duration FROM date1 TO date2 as [years,months,weeks,days]
/// for the given largestUnit ("year"/"month"/"week"/"day"). The ISO calendar is
/// just one calendar, so this is `cal_difference_date` at `Cal::Iso` — keeping a
/// second copy of the month-probing rule here is how the two drifted apart.
pub(crate) fn difference_iso_date(
    d1: (i64, i64, i64),
    d2: (i64, i64, i64),
    largest: &str,
) -> [i64; 4] {
    crate::vm::temporal::cal_difference_date(crate::vm::temporal::Cal::Iso, d1, d2, largest)
}

/// DifferenceISODateTime: duration FROM dt1 TO dt2 as a 10-field Duration for the
/// given largestUnit. Borrows a day when the time part runs opposite to the date
/// direction; folds everything into time units when largestUnit is sub-day.
pub(crate) fn difference_datetime(dt1: [i64; 9], dt2: [i64; 9], largest: &str) -> [i64; 10] {
    let time1 = time_to_ns(&[dt1[3], dt1[4], dt1[5], dt1[6], dt1[7], dt1[8]]);
    let time2 = time_to_ns(&[dt2[3], dt2[4], dt2[5], dt2[6], dt2[7], dt2[8]]);
    let mut time_diff = time2 - time1;
    let date1 = (dt1[0], dt1[1], dt1[2]);
    let e1 = iso_to_epoch_days(date1.0, date1.1, date1.2);
    let e2 = iso_to_epoch_days(dt2[0], dt2[1], dt2[2]);
    let date_sign = (e2 > e1) as i64 - (e2 < e1) as i64;
    let mut date2 = (dt2[0], dt2[1], dt2[2]);
    if time_diff != 0 && date_sign != 0 && time_diff.signum() != date_sign as i128 {
        let ed = iso_to_epoch_days(date2.0, date2.1, date2.2) - date_sign;
        date2 = epoch_days_to_iso(ed);
        time_diff += (date_sign as i128) * DAY_NS;
    }
    let mut df = [0i64; 10];
    if matches!(
        largest,
        "hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond"
    ) {
        let day_diff = iso_to_epoch_days(date2.0, date2.1, date2.2) - e1;
        let total = (day_diff as i128) * DAY_NS + time_diff;
        let units = [
            3_600_000_000_000i128, 60_000_000_000, 1_000_000_000, 1_000_000, 1_000, 1,
        ];
        let start = match largest {
            "hour" => 0,
            "minute" => 1,
            "second" => 2,
            "millisecond" => 3,
            "microsecond" => 4,
            _ => 5,
        };
        let sign = total.signum() as i64;
        let mut n = total.abs();
        for i in start..6 {
            df[4 + i] = (n / units[i]) as i64 * sign;
            n %= units[i];
        }
        return df;
    }
    let dpart = difference_iso_date(date1, date2, largest);
    df[..4].copy_from_slice(&dpart);
    let tsign = time_diff.signum() as i64;
    let t = ns_to_time(time_diff.abs());
    for i in 0..6 {
        df[4 + i] = t[i] * tsign;
    }
    df
}

/// Size of a time/day unit in nanoseconds (used for rounding).
/// Format the time portion "HH:MM[:SS[.fff]]" for a Temporal toString with a
/// resolved precision: `digits` = -1 (auto, trim trailing zeros), 0..9 fixed
/// fractional-second digits; `omit_sec` drops ":SS" (smallestUnit "minute").
pub(crate) fn format_time_part(t: &[i64; 6], digits: i32, omit_sec: bool) -> String {
    let mut out = format!("{:02}:{:02}", t[0], t[1]);
    if omit_sec {
        return out;
    }
    out.push_str(&format!(":{:02}", t[2]));
    let frac_ns = t[3] * 1_000_000 + t[4] * 1_000 + t[5]; // 0..=999_999_999
    if digits < 0 {
        if frac_ns != 0 {
            let s = format!("{frac_ns:09}");
            out.push('.');
            out.push_str(s.trim_end_matches('0'));
        }
    } else if digits > 0 {
        let s = format!("{frac_ns:09}");
        out.push('.');
        out.push_str(&s[..digits as usize]);
    }
    out
}

/// The ten Temporal duration units (singular), largest to smallest.
pub(crate) const DURATION_UNITS: &[&str] = &[
    "year", "month", "week", "day", "hour", "minute", "second", "millisecond", "microsecond",
    "nanosecond",
];

pub(crate) fn unit_ns(u: &str) -> i128 {
    match u {
        "day" => DAY_NS,
        "hour" => 3_600_000_000_000,
        "minute" => 60_000_000_000,
        "second" => 1_000_000_000,
        "millisecond" => 1_000_000,
        "microsecond" => 1_000,
        _ => 1, // nanosecond
    }
}

/// IsValidDuration: every field finite, the calendar fields below 2^32, and the
/// total time span (days..nanoseconds) strictly below 2^53 seconds in absolute
/// value. (Moved here from temporal.rs so balance_duration_ns can validate.)
pub(crate) fn is_valid_duration(f: &[f64; 10]) -> bool {
    if f.iter().any(|v| !v.is_finite()) {
        return false;
    }
    // Spec steps 1-2: every field must agree with the duration's overall sign
    // (the first non-zero field) — mixed-sign durations are invalid. NOTE:
    // f64::signum maps ±0.0 to ±1.0, so zeros must be classified explicitly.
    let sgn = |v: f64| {
        if v > 0.0 {
            1
        } else if v < 0.0 {
            -1
        } else {
            0
        }
    };
    let sign = f.iter().map(|&v| sgn(v)).find(|&s| s != 0).unwrap_or(0);
    if f.iter().any(|&v| sgn(v) != 0 && sgn(v) != sign) {
        return false;
    }
    let two_pow_32 = 4_294_967_296.0_f64; // 2^32
    if f[0].abs() >= two_pow_32 || f[1].abs() >= two_pow_32 || f[2].abs() >= two_pow_32 {
        return false;
    }
    // The bound is abs(days×86400 + h×3600 + m×60 + s + sub-seconds) < 2^53 s.
    // An f64 estimate decides everything except a thin band around 2^53 where
    // rounding is ambiguous (e.g. the spec maximum 2^53-1 + 0.999999999 rounds
    // up to 2^53 in f64); there, recompute the total exactly in i128 ns.
    let two53 = 9_007_199_254_740_992.0_f64; // 2^53
    let est = f[3] * 86_400.0
        + f[4] * 3_600.0
        + f[5] * 60.0
        + f[6]
        + f[7] / 1e3
        + f[8] / 1e6
        + f[9] / 1e9;
    // Margin must exceed the f64 ULP at 2^53 (which is 2.0, so 1.0 would vanish).
    if est.abs() >= two53 + 16.0 {
        return false;
    }
    if est.abs() <= two53 - 16.0 {
        return true;
    }
    // Ambiguous band. Exact i128 nanosecond total via checked arithmetic: a SINGLE
    // large sub-day field (e.g. milliseconds ≈ 9.007e18 ⇒ 9.007e15 s, a valid
    // duration) has an i128 product well within range, so it must NOT be rejected;
    // only a genuine i128 overflow (extreme/mixed-sign cancellation) is out of range.
    let total_ns: Option<i128> = (|| {
        let mul = |v: f64, scale: i128| (v as i128).checked_mul(scale);
        let mut acc = mul(f[3], 86_400_000_000_000)?;
        acc = acc.checked_add(mul(f[4], 3_600_000_000_000)?)?;
        acc = acc.checked_add(mul(f[5], 60_000_000_000)?)?;
        acc = acc.checked_add(mul(f[6], 1_000_000_000)?)?;
        acc = acc.checked_add(mul(f[7], 1_000_000)?)?;
        acc = acc.checked_add(mul(f[8], 1_000)?)?;
        acc = acc.checked_add(f[9] as i128)?;
        Some(acc)
    })();
    match total_ns {
        Some(ns) => ns.unsigned_abs() < 9_007_199_254_740_992u128 * 1_000_000_000,
        None => false,
    }
}

/// Decompose a signed nanosecond total into a Duration's day+time fields
/// [_,_,_,d,h,mi,s,ms,us,ns], from `largest` (day..nanosecond) down. Used by
/// Duration round/add/subtract without relativeTo (a day is exactly 24h).
///
/// TemporalDurationFromInternal: the largest (uncapped) component is stored as
/// a float64-representable integer — its exact i128 count rounds through f64
/// (so e.g. …551 becomes …552 above 2^53) — and the assembled record must
/// satisfy IsValidDuration (total time below 2^53 seconds), else RangeError.
/// The lower components are exact (each < 1000/60/24). The previous `as i64`
/// cast WRAPPED on overflow, letting out-of-range totals masquerade as valid.
pub(crate) fn balance_duration_ns(total_ns: i128, largest: &str) -> Result<[f64; 10], Thrown> {
    let sign = total_ns.signum() as f64;
    let mut n = total_ns.abs();
    let mut f = [0f64; 10];
    // (field index, unit size in ns) from day down to nanosecond.
    let units = [
        (3usize, DAY_NS),
        (4, 3_600_000_000_000),
        (5, 60_000_000_000),
        (6, 1_000_000_000),
        (7, 1_000_000),
        (8, 1_000),
        (9, 1),
    ];
    let start = match largest {
        "day" => 0,
        "hour" => 1,
        "minute" => 2,
        "second" => 3,
        "millisecond" => 4,
        "microsecond" => 5,
        _ => 6, // nanosecond
    };
    for (k, &(slot, sz)) in units[start..].iter().enumerate() {
        let q = n / sz;
        n %= sz;
        if k == 0 {
            // The largest (uncapped) component stores ℝ(𝔽(q)) — i128→f64 is
            // correctly rounded. Lower components are exact (< 1000/60/24).
            f[slot] = (q as f64) * sign;
        } else {
            f[slot] = q as f64 * sign;
        }
    }
    // IsValidDuration on the ROUNDED components: the f64 rounding of the
    // largest field can land exactly on the 2^53-seconds limit even when the
    // exact total was just below it.
    if !is_valid_duration(&f) {
        return Err(Thrown("RangeError: Temporal.Duration value out of range".into()));
    }
    Ok(f)
}

/// Maximum (exclusive) roundingIncrement for a time unit (the count of that unit
/// in the next-larger unit). None for "day" (no fixed maximum).
pub(crate) fn max_increment(u: &str) -> Option<i128> {
    match u {
        "hour" => Some(24),
        "minute" | "second" => Some(60),
        "millisecond" | "microsecond" | "nanosecond" => Some(1000),
        _ => None,
    }
}

/// Round `value` to a multiple of `inc` per a Temporal roundingMode.
pub(crate) fn round_increment(value: i128, inc: i128, mode: &str) -> i128 {
    if inc <= 1 && mode == "halfExpand" {
        // fast path: inc==1 rounds nothing
        if inc <= 1 {
            return value;
        }
    }
    if inc <= 0 {
        return value;
    }
    let lower = value.div_euclid(inc) * inc;
    let r = value - lower;
    if r == 0 {
        return value;
    }
    let upper = lower + inc;
    let expand = if value >= 0 { upper } else { lower }; // away from zero
    let trunc = if value >= 0 { lower } else { upper }; // toward zero
    match mode {
        "ceil" => upper,
        "floor" => lower,
        "expand" => expand,
        "trunc" => trunc,
        _ => {
            let twice = 2 * r;
            if twice != inc {
                if twice > inc {
                    upper
                } else {
                    lower
                }
            } else {
                match mode {
                    "halfCeil" => upper,
                    "halfFloor" => lower,
                    "halfTrunc" => trunc,
                    "halfEven" => {
                        if (lower / inc) % 2 == 0 {
                            lower
                        } else {
                            upper
                        }
                    }
                    _ => expand, // halfExpand (default)
                }
            }
        }
    }
}

/// RoundNumberToIncrementAsIfPositive: round `value` to a multiple of `inc`,
/// applying the rounding mode as if `value` were positive — so `trunc` behaves
/// like `floor` and `expand` like `ceil` regardless of sign ("rounding down is
/// toward the Big Bang, not the epoch"). Used when rounding an ABSOLUTE point on
/// the timeline (Temporal.Instant round/toString), as opposed to rounding a
/// duration (which uses the sign-relative `round_increment`). Implemented by
/// remapping the sign-relative modes to their absolute ceil/floor equivalents.
pub(crate) fn round_increment_as_if_positive(value: i128, inc: i128, mode: &str) -> i128 {
    let m = match mode {
        "expand" => "ceil",
        "trunc" => "floor",
        "halfExpand" => "halfCeil",
        "halfTrunc" => "halfFloor",
        other => other,
    };
    round_increment(value, inc, m)
}

/// The f64 nearest to `num / den` (`den != 0`), rounded to nearest with ties to
/// even — i.e. the SINGLE correctly-rounded double of the exact rational, as
/// `Temporal.Duration.prototype.total` requires. Casting the i128 numerator to
/// f64 first (`num as f64 / den as f64`) double-rounds and can be 1 ULP off once
/// the numerator exceeds 2^53; this divides exactly. Implemented by long division
/// to a 54-bit quotient (53-bit mantissa + one guard bit) carrying a sticky bit,
/// then a single round-half-even. The operands here keep every intermediate well
/// inside u128 (numerator < 2^84, denominator < 2^48 ⇒ scaled width ≤ ~105 bits).
pub(crate) fn rational_to_f64(num: i128, den: i128) -> f64 {
    if num == 0 {
        return 0.0;
    }
    let neg = (num < 0) ^ (den < 0);
    let a = num.unsigned_abs();
    let b = den.unsigned_abs();
    // floor(a * 2^shift / b) plus a sticky flag for the truncated remainder; the
    // quotient q represents q * 2^-shift ≈ a/b.
    let scaled_div = |shift: i32| -> (u128, bool) {
        if shift >= 0 {
            let n = a << (shift as u32);
            (n / b, n % b != 0)
        } else {
            let d = b << ((-shift) as u32);
            (a / d, a % d != 0)
        }
    };
    let bit_len = |x: u128| 128 - x.leading_zeros() as i32;
    // Aim for a 54-bit quotient (the bit estimate is within ±1 of the truth).
    let mut shift = 54 - (bit_len(a) - bit_len(b));
    let (mut q, mut sticky) = scaled_div(shift);
    while q >= (1u128 << 54) {
        sticky |= (q & 1) != 0;
        q >>= 1;
        shift -= 1;
    }
    while q < (1u128 << 53) {
        shift += 1;
        let (q2, s2) = scaled_div(shift);
        q = q2;
        sticky = s2;
    }
    // q is 54 bits; bit 0 is the round bit, the value is q * 2^-shift.
    let round_bit = q & 1;
    let mut mant = q >> 1; // 53-bit mantissa in [2^52, 2^53)
    let mut exp = 1 - shift; // value ≈ mant * 2^exp
    if round_bit == 1 && (sticky || (mant & 1) == 1) {
        mant += 1;
        if mant == (1u128 << 53) {
            mant >>= 1; // carry: 2^53 → 2^52
            exp += 1;
        }
    }
    // mant < 2^53 and 2^exp is a power of two, so the product is exact.
    let val = (mant as f64) * 2f64.powi(exp);
    if neg {
        -val
    } else {
        val
    }
}

/// Round a value lying at fractional position `progress` (0..1) between `lower`
/// and `lower+sign` to one of the two, per a Temporal roundingMode. `lower` is
/// the toward-zero neighbour; `progress==0` means the value is exactly `lower`.
/// Used for calendar-unit (year/month/week) difference rounding, where the
/// fraction comes from the sub-unit remainder against the anchor calendar.
pub(crate) fn round_fraction(lower: i64, sign: i64, progress: f64, mode: &str) -> i64 {
    if progress <= 0.0 {
        return lower; // exact
    }
    let upper = lower + sign;
    let pick_upper = match mode {
        "ceil" => sign > 0,
        "floor" => sign < 0,
        "trunc" => false,
        "expand" => true,
        _ => {
            if progress > 0.5 {
                true
            } else if progress < 0.5 {
                false
            } else {
                match mode {
                    "halfCeil" => sign > 0,
                    "halfFloor" => sign < 0,
                    "halfTrunc" => false,
                    "halfEven" => upper.rem_euclid(2) == 0,
                    _ => true, // halfExpand
                }
            }
        }
    };
    if pick_upper {
        upper
    } else {
        lower
    }
}

/// ISO day-of-week: Monday=1 … Sunday=7.
pub(crate) fn iso_day_of_week(y: i64, m: i64, d: i64) -> i64 {
    let ed = iso_to_epoch_days(y, m, d);
    (((ed % 7) + 3) % 7 + 7) % 7 + 1
}
/// Parse "YYYY-MM-DD" (optionally with time/zone/calendar suffix) → (y,m,d).
pub(crate) fn parse_iso_date(s: &str) -> Option<(i64, i64, i64)> {
    let s = s.trim();
    // Optional leading sign for expanded years (±YYYYYY).
    let bytes = s.as_bytes();
    let (sign, rest) = match bytes.first() {
        Some(b'-') | Some(b'+') => (if bytes[0] == b'-' { -1i64 } else { 1 }, &s[1..]),
        _ => (1, s),
    };
    // Year: 4 digits, OR 6 for an expanded year — but an expanded ±YYYYYY year
    // REQUIRES the sign, so without one the year is always 4 digits (this lets
    // basic-format dates like "19761118" parse as 1976-11-18, not year 197611).
    let signed = matches!(bytes.first(), Some(b'-') | Some(b'+'));
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let ylen = if signed && digits.len() >= 6 { 6 } else { 4 };
    if digits.len() < ylen {
        return None;
    }
    let yv = rest[..ylen].parse::<i64>().ok()?;
    // Negative zero as an extended year ("-000000") is rejected.
    if sign < 0 && yv == 0 {
        return None;
    }
    let y = sign * yv;
    let after = &rest[ylen..];
    let had_ym_sep = after.starts_with('-');
    let after = after.strip_prefix('-').unwrap_or(after);
    if after.len() < 2 {
        return None;
    }
    let m = after[..2].parse::<i64>().ok()?;
    let after = &after[2..];
    let had_md_sep = after.starts_with('-');
    let after = after.strip_prefix('-').unwrap_or(after);
    if after.len() < 2 {
        return None;
    }
    let d = after[..2].parse::<i64>().ok()?;
    // The year-month and month-day separators must match — both '-' (extended) or
    // both absent (basic). Reject mixed forms "2020-0101" / "202001-01".
    if had_ym_sep != had_md_sep {
        return None;
    }
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    // Validate any trailing content: a date-only string may be followed only by a
    // calendar/annotation block "[...]" or a "T" + valid time. A bare UTC
    // designator/offset (Z, +HH:MM) or a "T" with no time is invalid.
    let rem = &after[2..];
    if !rem.is_empty() {
        if rem.starts_with('[') {
            // calendar/time-zone annotations — accepted (not deeply validated)
        } else if rem.starts_with(['T', 't', ' ']) {
            // DateTimeSeparator is `T` | `t` | <space>; a valid time must follow.
            if parse_iso_time(&rem[1..]).is_none() {
                return None;
            }
        } else {
            return None;
        }
    }
    Some((y, m, d))
}

/// ISO-8601 serialization of a Temporal.Duration (`P1Y2M3DT4H5.5S`). ms/us/ns
/// fold into fractional seconds. All-zero → "PT0S".
pub(crate) fn duration_to_string(f: &[f64; 10]) -> String {
    // The auto-precision path never rounds, so a duration that was valid at
    // construction can never overflow here — fall back defensively to "PT0S".
    duration_to_string_opts(f, -1, "trunc").unwrap_or_else(|| "PT0S".to_string())
}

/// Like `duration_to_string` but with a toString precision: `digits` = -1 (auto,
/// trailing zeros trimmed) or 0..9 fixed fractional-second digits (the seconds
/// component is then always shown), with `mode` rounding the sub-second part.
///
/// Mirrors TemporalDurationToString: the time portion is rounded to the
/// precision increment and then balanced UP TO `DefaultTemporalLargestUnit`
/// (the largest non-zero unit) — so a rounding carry can cross seconds→minutes→
/// hours→days, and a day-carry is folded into the date `days` field, but units
/// never balance past days into weeks/months/years. The balanced result is
/// re-validated against IsValidDuration's `abs < 2^53 s` time bound; `None`
/// signals the caller to throw a RangeError.
pub(crate) fn duration_to_string_opts(f: &[f64; 10], digits: i32, mode: &str) -> Option<String> {
    let sign = f
        .iter()
        .map(|&x| {
            if x > 0.0 {
                1
            } else if x < 0.0 {
                -1
            } else {
                0
            }
        })
        .find(|&s| s != 0)
        .unwrap_or(0);
    // Integer-valued f64 → i128 is exact (the max legal field, ~9.007e24 ns,
    // is far below 2^127).
    let a: Vec<i128> = f.iter().map(|&x| x.abs() as i128).collect();
    let (y, mo, w, mut d) = (a[0], a[1], a[2], a[3]);
    // RoundTimeDuration + balancing run only when the precision actually rounds —
    // i.e. precision.[[Unit]] ≠ "nanosecond" OR increment ≠ 1 ns, which is exactly
    // `digits` in 0..=8. Auto (-1) and nanosecond (9) skip step 10 entirely and
    // render the stored fields literally (sub-seconds still fold up into seconds).
    let (hours, mins, sec_ns) = if (0..=8).contains(&digits) {
        // DefaultTemporalLargestUnit: index of the largest non-zero unit
        // (0=Y, 1=Mo, 2=W, 3=D, 4=H, 5=Mi, 6=S, 7=ms, 8=us, 9=ns; all-zero ⇒ ns).
        let li = (0..10).find(|&i| a[i] != 0).unwrap_or(9);
        // Full time-portion nanoseconds (hours down to nanoseconds), rounded.
        let total_ns = round_increment(
            a[4] * 3_600_000_000_000
                + a[5] * 60_000_000_000
                + a[6] * 1_000_000_000
                + a[7] * 1_000_000
                + a[8] * 1_000
                + a[9],
            10i128.pow(9 - digits as u32),
            mode,
        );
        // CreateDurationRecord → IsValidDuration: the rounded time plus the
        // (unbalanced) date days must keep abs(days×86400 + time-seconds) < 2^53 s,
        // else RangeError (signalled to the caller as None).
        const MAX_TIME_NS: i128 = 9_007_199_254_740_992 * 1_000_000_000; // 2^53 × 1e9
        if d * 86_400_000_000_000 + total_ns >= MAX_TIME_NS {
            return None;
        }
        // TemporalDurationFromInternal: distribute the rounded nanoseconds top-down
        // into only the units at or below `largestUnit`; the highest allowed unit is
        // uncapped, and a day-carry (largestUnit ≥ day) folds into the date days.
        let mut rem = total_ns;
        if li <= 3 {
            d += rem / 86_400_000_000_000;
            rem %= 86_400_000_000_000;
        }
        let hours = if li <= 4 {
            let v = rem / 3_600_000_000_000;
            rem %= 3_600_000_000_000;
            v
        } else {
            0
        };
        let mins = if li <= 5 {
            let v = rem / 60_000_000_000;
            rem %= 60_000_000_000;
            v
        } else {
            0
        };
        (hours, mins, rem)
    } else {
        // Literal: hours/minutes as stored; sub-seconds fold up into seconds only.
        (
            a[4],
            a[5],
            a[6] * 1_000_000_000 + a[7] * 1_000_000 + a[8] * 1_000 + a[9],
        )
    };
    let whole_s = sec_ns / 1_000_000_000;
    let frac_ns = (sec_ns % 1_000_000_000) as u64;
    let mut out = String::new();
    if sign < 0 {
        out.push('-');
    }
    out.push('P');
    if y != 0 {
        out.push_str(&format!("{y}Y"));
    }
    if mo != 0 {
        out.push_str(&format!("{mo}M"));
    }
    if w != 0 {
        out.push_str(&format!("{w}W"));
    }
    if d != 0 {
        out.push_str(&format!("{d}D"));
    }
    let show_seconds = whole_s != 0 || frac_ns != 0 || digits >= 0;
    let has_time = hours != 0 || mins != 0 || show_seconds;
    if has_time {
        out.push('T');
        if hours != 0 {
            out.push_str(&format!("{hours}H"));
        }
        if mins != 0 {
            out.push_str(&format!("{mins}M"));
        }
        if show_seconds {
            if digits > 0 {
                let frac = format!("{frac_ns:09}");
                out.push_str(&format!("{whole_s}.{}S", &frac[..digits as usize]));
            } else if digits == 0 || frac_ns == 0 {
                out.push_str(&format!("{whole_s}S"));
            } else {
                let frac = format!("{frac_ns:09}");
                out.push_str(&format!("{whole_s}.{}S", frac.trim_end_matches('0')));
            }
        }
    }
    if out == "P" || out == "-P" {
        return Some("PT0S".to_string());
    }
    Some(out)
}

/// Parse an ISO-8601 duration string into `[y,mo,w,d,h,mi,s,ms,us,ns]`. Handles
/// integer date/time units and a fractional seconds field. `None` if malformed.
pub(crate) fn parse_iso_duration(s: &str) -> Option<[i64; 10]> {
    let s = s.trim();
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => (-1i64, r),
        None => (1, s.strip_prefix('+').unwrap_or(s)),
    };
    let rest = rest.strip_prefix(['P', 'p'])?;
    let mut f = [0i64; 10];
    let (date_s, time_s) = match rest.find(['T', 't']) {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    let mut saw = false;
    // Date units Y/M/W/D.
    let mut num = String::new();
    for c in date_s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            let n: i64 = num.parse().ok()?;
            num.clear();
            let slot = match c {
                'Y' | 'y' => 0,
                'M' | 'm' => 1, // months (designators are ASCII-case-insensitive)
                'W' | 'w' => 2,
                'D' | 'd' => 3,
                _ => return None,
            };
            f[slot] = n;
            saw = true;
        }
    }
    if !num.is_empty() {
        return None;
    }
    // Time units H/M/S. Only the lowest-order unit present may carry a fraction
    // (`,`/`.`), which cascades into the smaller units (e.g. PT1.5H → 1h 30m).
    if !time_s.is_empty() {
        let mut num = String::new();
        let mut frac = String::new();
        let mut in_frac = false;
        let mut frac_done = false; // a fractional unit must be the last one
        for c in time_s.chars() {
            if c.is_ascii_digit() {
                if in_frac {
                    frac.push(c);
                } else {
                    num.push(c);
                }
            } else if (c == '.' || c == ',') && !in_frac {
                in_frac = true;
            } else {
                if frac_done || num.is_empty() {
                    return None; // a unit after a fractional one, or a bare unit
                }
                let n: i64 = num.parse().ok()?;
                // (slot, seconds-per-unit) for H/M/S.
                let (slot, unit_secs): (usize, i128) = match c {
                    'H' | 'h' => (4, 3_600),
                    'M' | 'm' => (5, 60), // minutes (case-insensitive)
                    'S' | 's' => (6, 1),
                    _ => return None,
                };
                f[slot] = n;
                saw = true;
                if in_frac {
                    if frac.is_empty() {
                        return None; // a "," / "." with no fractional digits
                    }
                    // TemporalDurationString grammar: at most 9 fraction digits
                    // (sub-nanosecond digits are a syntax error, not rounding).
                    if frac.len() > 9 {
                        return None;
                    }
                    let l = frac.len() as u32;
                    let numer: i128 = frac.parse().ok()?;
                    let denom: i128 = 10i128.pow(l);
                    let scaled = numer * unit_secs * 1_000_000_000;
                    let sub_ns = (scaled + denom / 2) / denom;
                    // Distribute into min/s/ms/us/ns (indices 5..=9), starting at
                    // the unit just below the fractional one.
                    let divisors = [60_000_000_000i128, 1_000_000_000, 1_000_000, 1_000, 1];
                    let start = match c {
                        'H' | 'h' => 0,
                        'M' | 'm' => 1, // designators are case-insensitive
                        _ => 2,         // S
                    };
                    let mut rem = sub_ns;
                    for (k, &div) in divisors.iter().enumerate().skip(start) {
                        f[5 + k] = (rem / div) as i64;
                        rem %= div;
                    }
                    frac_done = true;
                }
                num.clear();
                frac.clear();
                in_frac = false;
            }
        }
        if !num.is_empty() || in_frac {
            return None;
        }
    }
    if !saw {
        return None;
    }
    if sign < 0 {
        for x in f.iter_mut() {
            *x = -*x;
        }
    }
    Some(f)
}

#[cfg(test)]
mod rational_to_f64_tests {
    use super::rational_to_f64;

    #[test]
    fn matches_hardware_division_in_exact_range() {
        // For operands below 2^53 both casts are exact and IEEE-754 division is
        // already correctly rounded, so it is a trustworthy oracle. A small LCG
        // gives deterministic coverage of the rounding/tie logic.
        let mut s: u64 = 0x9E3779B97F4A7C15;
        let mut next = || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 11) as i128 // 53 random-ish bits
        };
        for _ in 0..200_000 {
            let a = (next() % ((1i128 << 52) - 1)) + 1;
            let b = (next() % ((1i128 << 52) - 1)) + 1;
            let oracle = a as f64 / b as f64;
            assert_eq!(rational_to_f64(a, b), oracle, "a={a} b={b}");
            // Sign must be respected exactly.
            assert_eq!(rational_to_f64(-a, b), -oracle, "neg a={a} b={b}");
            assert_eq!(rational_to_f64(a, -b), -oracle, "neg b a={a} b={b}");
        }
    }

    #[test]
    fn exact_large_integers_are_exact() {
        // q * b / b == q with no rounding, for q exactly representable.
        for &q in &[0i128, 1, 2, 1_000_000, (1i128 << 53) - 1, 1i128 << 53] {
            for &b in &[1i128, 3, 1_000_000_000, 3_600_000_000_000] {
                assert_eq!(rational_to_f64(q * b, b), q as f64, "q={q} b={b}");
            }
        }
    }

    #[test]
    fn matches_test262_precision_cases() {
        // The exact rationals from the failing Duration.total precision tests; the
        // expected values are the single correctly-rounded doubles.
        // 4000 h + 1 ns, in hours.
        assert_eq!(rational_to_f64(14_400_000_000_000_001, 3_600_000_000_000), 4000.000_000_000_000_5);
        // (2^51 s + 200 ms) totalled in seconds → the .2 fraction vanishes at that
        // magnitude (ULP is 0.5, so it rounds down to even).
        let ns = (1i128 << 51) * 1_000_000_000 + 200_000_000;
        assert_eq!(rational_to_f64(ns, 1_000_000_000), 2_251_799_813_685_248.0);
        // A large numerator that the naive double-cast drops a ULP on.
        assert_eq!(rational_to_f64(28_171_865_665_040_770, 3_600_000_000_000), 7825.518_240_289_103);
    }
}

