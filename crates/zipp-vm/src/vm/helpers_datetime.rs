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
    let digits: Vec<char> = s.chars().collect();
    let take2 = |i: usize| -> Option<i64> {
        if i + 1 < digits.len() && digits[i].is_ascii_digit() && digits[i + 1].is_ascii_digit() {
            format!("{}{}", digits[i], digits[i + 1]).parse().ok()
        } else {
            None
        }
    };
    let h = take2(0)?;
    // minute after optional ':'
    let mut i = 2;
    if digits.get(i) == Some(&':') {
        i += 1;
    }
    let mi = take2(i).unwrap_or(0);
    i += 2;
    let mut sec = 0i64;
    let mut sub = [0i64; 3];
    if digits.get(i) == Some(&':') || digits.get(i).is_some_and(|c| c.is_ascii_digit()) {
        if digits.get(i) == Some(&':') {
            i += 1;
        }
        sec = take2(i).unwrap_or(0);
        i += 2;
        if digits.get(i) == Some(&'.') || digits.get(i) == Some(&',') {
            i += 1;
            let mut fr = String::new();
            while let Some(c) = digits.get(i) {
                if c.is_ascii_digit() {
                    fr.push(*c);
                    i += 1;
                } else {
                    break;
                }
            }
            while fr.len() < 9 {
                fr.push('0');
            }
            fr.truncate(9);
            let ns: i64 = fr.parse().ok()?;
            sub = [ns / 1_000_000, (ns / 1_000) % 1_000, ns % 1_000];
        }
    }
    if !(0..24).contains(&h) || !(0..60).contains(&mi) || !(0..60).contains(&sec) {
        return None;
    }
    Some([h, mi, sec, sub[0], sub[1], sub[2]])
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
        _ => [0; 6],
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
    let body = &s[1..];
    let digits: String = body.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 2 {
        return None;
    }
    let h: i128 = digits[..2].parse().ok()?;
    let mi: i128 = if digits.len() >= 4 { digits[2..4].parse().ok()? } else { 0 };
    Some(sign * (h * 3_600_000_000_000 + mi * 60_000_000_000))
}

/// Parse an ISO instant string ("…Z" or "…±HH:MM") → epoch nanoseconds (UTC).
pub(crate) fn instant_str_to_ns(s: &str) -> Option<i128> {
    let s = s.trim();
    let tpos = s.find(['T', 't'])?;
    let after_t = &s[tpos + 1..];
    // Locate the offset/Z that ends the time part.
    let (dt_part, off): (&str, Option<&str>) = if let Some(z) = after_t.find(['Z', 'z']) {
        (&s[..tpos + 1 + z], Some("Z"))
    } else if let Some(rel) = after_t.find('+') {
        (&s[..tpos + 1 + rel], Some(&after_t[rel..]))
    } else if let Some(rel) = after_t.find('-') {
        (&s[..tpos + 1 + rel], Some(&after_t[rel..]))
    } else {
        return None; // an Instant string must carry a UTC designator
    };
    let dt = parse_iso_datetime(dt_part)?;
    let mut ns = (iso_to_epoch_days(dt[0], dt[1], dt[2]) as i128) * DAY_NS
        + time_to_ns(&[dt[3], dt[4], dt[5], dt[6], dt[7], dt[8]]);
    ns -= parse_offset_ns(off?)?;
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
pub(crate) fn parse_month_code(s: &str) -> Option<i64> {
    let body = s.strip_prefix('M')?;
    let body = body.strip_suffix('L').unwrap_or(body);
    let n = body.parse::<i64>().ok()?;
    (1..=12).contains(&n).then_some(n)
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
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let ylen = if digits.len() >= 6 { 6 } else { 4 };
    if digits.len() < ylen {
        return None;
    }
    let y = sign * rest[..ylen].parse::<i64>().ok()?;
    let after = &rest[ylen..];
    let after = after.strip_prefix('-').unwrap_or(after);
    if after.len() < 2 {
        return None;
    }
    let m = after[..2].parse::<i64>().ok()?;
    if !(1..=12).contains(&m) {
        return None;
    }
    Some((y, m, 1))
}

/// Parse "MM-DD" / "--MM-DD" (or a fuller ISO date) → (referenceISOYear, month, day).
pub(crate) fn parse_iso_month_day(s: &str) -> Option<(i64, i64, i64)> {
    let s = s.trim();
    if let Some((y, m, d)) = parse_iso_date(s) {
        return Some((y, m, d));
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
    Some((1972, m, d))
}

/// Canonicalize a BCP-47 language tag (pragmatic: validates structure, lowercases
/// the language, Titlecases a 4-letter script subtag, uppercases a 2-letter region;
/// subtags after a singleton extension are lowercased). Returns None if malformed.
pub(crate) fn canonicalize_locale(tag: &str) -> Option<String> {
    let tag = tag.trim();
    if tag.is_empty() {
        return None;
    }
    let parts: Vec<&str> = tag.split('-').collect();
    for p in &parts {
        if p.is_empty() || p.len() > 8 || !p.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
    }
    let lang = parts[0];
    let ll = lang.len();
    if !(lang.chars().all(|c| c.is_ascii_alphabetic()) && ((2..=3).contains(&ll) || (5..=8).contains(&ll)))
    {
        return None;
    }
    let mut out = vec![lang.to_ascii_lowercase()];
    let mut seen_singleton = false;
    for p in &parts[1..] {
        if p.len() == 1 {
            seen_singleton = true;
            out.push(p.to_ascii_lowercase());
        } else if seen_singleton {
            out.push(p.to_ascii_lowercase());
        } else if p.len() == 4 && p.chars().all(|c| c.is_ascii_alphabetic()) {
            let s = p.to_ascii_lowercase();
            out.push(format!("{}{}", s[..1].to_ascii_uppercase(), &s[1..]));
        } else if (p.len() == 2 && p.chars().all(|c| c.is_ascii_alphabetic()))
            || (p.len() == 3 && p.chars().all(|c| c.is_ascii_digit()))
        {
            out.push(p.to_ascii_uppercase());
        } else {
            out.push(p.to_ascii_lowercase());
        }
    }
    Some(out.join("-"))
}

/// Format a number for Intl.NumberFormat with grouping + min/max fraction digits.
/// (en-US conventions: "," grouping, "." decimal; percent multiplies by 100.)
pub(crate) fn format_number_intl(
    n: f64,
    style: &str,
    min_frac: i64,
    max_frac: i64,
    min_int: i64,
    grouping: bool,
) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    let neg = n.is_sign_negative() && n != 0.0;
    let mut x = n.abs();
    if style == "percent" {
        x *= 100.0;
    }
    if x.is_infinite() {
        let mut s = "∞".to_string();
        if style == "percent" {
            s.push('%');
        }
        if neg {
            s.insert(0, '-');
        }
        return s;
    }
    let factor = 10f64.powi(max_frac.clamp(0, 100) as i32);
    let rounded = (x * factor).round() / factor;
    let s = format!("{:.*}", max_frac.clamp(0, 100) as usize, rounded);
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => (i.to_string(), f.to_string()),
        None => (s.clone(), String::new()),
    };
    let mut frac = frac_part;
    while frac.len() as i64 > min_frac && frac.ends_with('0') {
        frac.pop();
    }
    let mut int_digits = int_part;
    while (int_digits.len() as i64) < min_int {
        int_digits.insert(0, '0');
    }
    let grouped = if grouping && int_digits.len() > 3 {
        let n = int_digits.len();
        let first = match n % 3 {
            0 => 3,
            r => r,
        };
        let mut out = String::from(&int_digits[..first]);
        let mut i = first;
        while i < n {
            out.push(',');
            out.push_str(&int_digits[i..i + 3]);
            i += 3;
        }
        out
    } else {
        int_digits
    };
    let mut res = grouped;
    if !frac.is_empty() {
        res.push('.');
        res.push_str(&frac);
    }
    if style == "percent" {
        res.push('%');
    }
    if neg {
        res.insert(0, '-');
    }
    res
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

/// en-US list formatting: "a", "a and b", "a, b, and c" (conj = "and"/"or").
pub(crate) fn format_list_en(items: &[String], conj: &str) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        2 => format!("{} {} {}", items[0], conj, items[1]),
        n => {
            let head = items[..n - 1].join(", ");
            format!("{head}, {conj} {}", items[n - 1])
        }
    }
}

/// en-US relative-time formatting (numeric "always"): "in N units" / "N units ago".
pub(crate) fn format_relative_time_en(value: f64, unit: &str) -> String {
    let base = unit.strip_suffix('s').unwrap_or(unit);
    let n = value.abs();
    let plural = (n - 1.0).abs() > f64::EPSILON; // anything but exactly 1 → plural
    let unit_str = if plural { format!("{base}s") } else { base.to_string() };
    let num = if n.fract() == 0.0 { format!("{}", n as i64) } else { format!("{n}") };
    if value < 0.0 {
        format!("{num} {unit_str} ago")
    } else {
        format!("in {num} {unit_str}")
    }
}

/// Minimal en duration formatting: non-zero fields joined as "N unit, …".
pub(crate) fn format_duration_en(d: &[i64; 10]) -> String {
    const NAMES: [&str; 10] = [
        "yr", "mth", "wk", "day", "hr", "min", "sec", "ms", "μs", "ns",
    ];
    let parts: Vec<String> = d
        .iter()
        .enumerate()
        .filter(|(_, &v)| v != 0)
        .map(|(i, &v)| format!("{v} {}", NAMES[i]))
        .collect();
    if parts.is_empty() {
        "0 sec".to_string()
    } else {
        parts.join(", ")
    }
}

/// Normalize a Temporal unit option: strip a trailing plural "s"; "auto"→`auto_to`.
pub(crate) fn normalize_unit(u: &str, auto_to: &str) -> String {
    let base = u.strip_suffix('s').unwrap_or(u);
    if base == "auto" { auto_to.to_string() } else { base.to_string() }
}

/// DifferenceISODate: the duration FROM date1 TO date2 as [years,months,weeks,days]
/// for the given largestUnit ("year"/"month"/"week"/"day"). Sign-aware. Mirrors the
/// Temporal reference algorithm (constrain-add probing for the year/month split).
pub(crate) fn difference_iso_date(
    d1: (i64, i64, i64),
    d2: (i64, i64, i64),
    largest: &str,
) -> [i64; 4] {
    let cmp = |a: (i64, i64, i64), b: (i64, i64, i64)| -> i64 {
        let (ea, eb) = (iso_to_epoch_days(a.0, a.1, a.2), iso_to_epoch_days(b.0, b.1, b.2));
        (ea > eb) as i64 - (ea < eb) as i64
    };
    if largest == "day" || largest == "week" {
        let mut days = iso_to_epoch_days(d2.0, d2.1, d2.2) - iso_to_epoch_days(d1.0, d1.1, d1.2);
        let mut weeks = 0;
        if largest == "week" {
            weeks = days / 7;
            days %= 7;
        }
        return [0, 0, weeks, days];
    }
    // year / month
    let sign = -cmp(d1, d2); // +1 if d2 after d1
    if sign == 0 {
        return [0, 0, 0, 0];
    }
    let (y1, m1, dd1) = d1;
    // (y1,m1,dd1) + (yy years, mm months), day constrained to the target month.
    let add_ym = |yy: i64, mm: i64| -> (i64, i64, i64) {
        let total = y1 * 12 + (m1 - 1) + yy * 12 + mm;
        let ny = total.div_euclid(12);
        let nm = total.rem_euclid(12) + 1;
        (ny, nm, dd1.min(days_in_month(ny, nm)))
    };
    let mut years = d2.0 - y1;
    let mut mid = add_ym(years, 0);
    if -cmp(mid, d2) == 0 {
        return if largest == "year" { [years, 0, 0, 0] } else { [0, years * 12, 0, 0] };
    }
    let mut months = d2.1 - m1;
    if -cmp(mid, d2) != sign {
        years -= sign;
        months += sign * 12;
    }
    mid = add_ym(years, months);
    let mid_sign = -cmp(mid, d2);
    if mid_sign == 0 {
        return if largest == "year" {
            [years, months, 0, 0]
        } else {
            [0, years * 12 + months, 0, 0]
        };
    }
    if mid_sign != sign {
        months -= sign;
        mid = add_ym(years, months);
    }
    let days = iso_to_epoch_days(d2.0, d2.1, d2.2) - iso_to_epoch_days(mid.0, mid.1, mid.2);
    if largest == "year" {
        [years, months, 0, days]
    } else {
        [0, years * 12 + months, 0, days]
    }
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
    // Year: 4 digits (or 6 for expanded). Then "-MM-DD" (separators optional).
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let ylen = if digits.len() >= 6 { 6 } else { 4 };
    if digits.len() < ylen {
        return None;
    }
    let y = sign * rest[..ylen].parse::<i64>().ok()?;
    let after = &rest[ylen..];
    let after = after.strip_prefix('-').unwrap_or(after);
    if after.len() < 2 {
        return None;
    }
    let m = after[..2].parse::<i64>().ok()?;
    let after = &after[2..];
    let after = after.strip_prefix('-').unwrap_or(after);
    if after.len() < 2 {
        return None;
    }
    let d = after[..2].parse::<i64>().ok()?;
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    Some((y, m, d))
}

/// ISO-8601 serialization of a Temporal.Duration (`P1Y2M3DT4H5.5S`). ms/us/ns
/// fold into fractional seconds. All-zero → "PT0S".
pub(crate) fn duration_to_string(f: &[i64; 10]) -> String {
    let sign = f.iter().map(|x| x.signum()).find(|&s| s != 0).unwrap_or(0);
    let a: Vec<i128> = f.iter().map(|x| (*x as i128).abs()).collect();
    let (y, mo, w, d, h, mi) = (a[0], a[1], a[2], a[3], a[4], a[5]);
    let total_ns = a[6] * 1_000_000_000 + a[7] * 1_000_000 + a[8] * 1_000 + a[9];
    let whole_s = total_ns / 1_000_000_000;
    let frac_ns = (total_ns % 1_000_000_000) as u64;
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
    let has_time = h != 0 || mi != 0 || whole_s != 0 || frac_ns != 0;
    if has_time {
        out.push('T');
        if h != 0 {
            out.push_str(&format!("{h}H"));
        }
        if mi != 0 {
            out.push_str(&format!("{mi}M"));
        }
        if whole_s != 0 || frac_ns != 0 {
            if frac_ns == 0 {
                out.push_str(&format!("{whole_s}S"));
            } else {
                let frac = format!("{frac_ns:09}");
                out.push_str(&format!("{whole_s}.{}S", frac.trim_end_matches('0')));
            }
        }
    }
    if out == "P" || out == "-P" {
        return "PT0S".to_string();
    }
    out
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
                'M' => 1,
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
    // Time units H/M/S (S may have a fraction → ms/us/ns).
    if !time_s.is_empty() {
        let mut num = String::new();
        let mut frac = String::new();
        let mut in_frac = false;
        for c in time_s.chars() {
            if c.is_ascii_digit() {
                if in_frac {
                    frac.push(c);
                } else {
                    num.push(c);
                }
            } else if c == '.' || c == ',' {
                in_frac = true;
            } else {
                let n: i64 = num.parse().ok()?;
                let slot = match c {
                    'H' | 'h' => 4,
                    'M' => 5,
                    'S' | 's' => 6,
                    _ => return None,
                };
                f[slot] = n;
                saw = true;
                if !frac.is_empty() {
                    if !matches!(c, 'S' | 's') {
                        return None; // only seconds-fraction supported
                    }
                    let mut fr = frac.clone();
                    while fr.len() < 9 {
                        fr.push('0');
                    }
                    fr.truncate(9);
                    let ns: i64 = fr.parse().ok()?;
                    f[7] = ns / 1_000_000;
                    f[8] = (ns / 1_000) % 1_000;
                    f[9] = ns % 1_000;
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

