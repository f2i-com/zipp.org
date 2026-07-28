#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

/// The result of one PrepareTemporalFields pass over a PlainDateTime-like
/// property bag (see [`Vm::read_pdt_bag`]): the raw slot-mapped fields, the
/// deferred monthCode validity, and — for ZonedDateTime-like bags — the
/// validated `offset` (ns) and the raw `timeZone` value.
pub(crate) struct PdtBag {
    pub(crate) f: [i64; 9],
    /// The month as read — an ordinal `month` or a `monthCode`; which ordinal a
    /// code names is only decidable once `finish_pdt_fields` has the year.
    month: MonthRef,
    month_code_invalid: bool,
    /// The numeric `month` when a `monthCode` was ALSO given (the agreement check
    /// is year-dependent in a leap-month calendar).
    month_conflict: Option<i64>,
    pub(crate) bag_off: Option<i64>,
    pub(crate) tz: Value,
    /// The calendar the bag's `calendar` field selected, plus the calendar-space
    /// year inputs it carried. `f[0..3]` holds the RAW year/month/day as read;
    /// `finish_pdt_fields` resolves them into the stored ISO date.
    pub(crate) cal: Cal,
    era: Option<String>,
    era_year: Option<i64>,
    year: Option<i64>,
}


/// `IsValidDuration` (spec): all fields finite, |years|/|months|/|weeks| < 2^32,
/// and the combined days+time span is under 2^53 seconds. Operates on the raw
/// f64 fields so out-of-range magnitudes are caught before any i64 truncation.
/// Validate the `[...]` annotation suffix of a Temporal ISO string per the
/// grammar's critical-flag rules: a critical annotation with an unknown key
/// (anything but `u-ca`) is rejected, at most one time-zone annotation is
/// allowed, and 2+ calendar (`u-ca`) annotations are rejected if any is
/// critical. `ann` starts at the first `[`.
fn annotations_valid(ann: &str) -> bool {
    let mut s = ann;
    let mut cal_count = 0u32;
    let mut cal_critical = false;
    let mut tz_count = 0u32;
    while !s.is_empty() {
        if !s.starts_with('[') {
            return false;
        }
        let end = match s.find(']') {
            Some(e) => e,
            None => return false,
        };
        let content = &s[1..end];
        s = &s[end + 1..];
        let (critical, body) = match content.strip_prefix('!') {
            Some(b) => (true, b),
            None => (false, content),
        };
        if let Some(eq) = body.find('=') {
            let key = &body[..eq];
            // AnnotationKey grammar: (a-z | _) then (a-z | _ | 0-9 | -)*. An
            // upper-cased or otherwise malformed key (e.g. "U-CA", "FOO") is a
            // syntax error — NOT an ignorable unknown annotation.
            let key_char = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_';
            let valid_key = key.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
                && key.bytes().all(key_char);
            if !valid_key {
                return false;
            }
            if key == "u-ca" {
                cal_count += 1;
                cal_critical |= critical;
            } else if critical {
                return false; // critical unknown key annotation
            }
        } else {
            // [Area/Location] or [±HH:MM] offset time-zone annotation. An offset-form
            // annotation must be minute precision (±HH, ±HH:MM, ±HHMM) — a sub-minute
            // one ([-07:00:01] / [-070001] / [-07:00:00.1]) is invalid.
            if let Some(off) = body.strip_prefix(['+', '-']) {
                let sub_minute = off.contains('.')
                    || off.contains(',')
                    || off.matches(':').count() >= 2
                    || (!off.contains(':') && off.bytes().filter(|c| c.is_ascii_digit()).count() > 4);
                if sub_minute {
                    return false;
                }
            }
            tz_count += 1;
        }
    }
    tz_count <= 1 && !(cal_count > 1 && cal_critical)
}

/// Validate a Temporal ISO string for a given parser context: the annotation
/// suffix must be well-formed, and (for the wall-clock "Plain" types) the string
/// must not carry a `Z`/`z` UTC designator (a numeric offset is still allowed).
/// `require_known_calendar` (for the calendar-BEARING types — PlainDate/DateTime/
/// YearMonth/MonthDay/ZonedDateTime/Duration-relativeTo) additionally requires the
/// FIRST `[u-ca=…]` calendar annotation, if present, to name a calendar this
/// engine implements (so "…[u-ca=notacal]" / a date-like calendar name is a
/// RangeError). The calendar-LESS types (Instant, PlainTime) ignore it entirely.
fn temporal_string_ok(s: &str, reject_utc_designator: bool, require_known_calendar: bool) -> bool {
    let s = s.trim();
    let (main, ann) = match s.find('[') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    if !ann.is_empty() && !annotations_valid(ann) {
        return false;
    }
    // The first calendar annotation is the resolved calendar; later `[u-ca=…]` are
    // ignored. It must name a calendar this engine implements.
    if require_known_calendar {
        if let Some(p) = ann.find("u-ca=") {
            let val = &ann[p + 5..];
            match val.find(']') {
                Some(end) if calendar_by_id(&val[..end]).is_some() => {}
                _ => return false,
            }
        }
    }
    if reject_utc_designator && main.bytes().any(|b| b == b'Z' || b == b'z') {
        return false;
    }
    // No fractional run (sub-second OR a sub-minute offset) may exceed 9 digits.
    let chars: Vec<char> = main.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '.' || c == ',' {
            let n = chars[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
            if n > 9 {
                return false;
            }
        }
    }
    true
}

/// Resolve a calendar string to its calendar id (not yet checked for support):
/// a bare identifier in any ASCII case, a `[u-ca=…]` annotation embedded in an
/// ISO string, or a bare ISO date / year-month / month-day string (→ iso8601).
fn calendar_id_from_string(s: &str) -> Option<String> {
    let s = s.trim();
    if calendar_by_id(s).is_some() {
        return Some(s.to_string());
    }
    if let Some(p) = s.find("u-ca=") {
        let val = &s[p + 5..];
        let end = val.find(']')?;
        return Some(val[..end].to_string());
    }
    if parse_iso_datetime(s).is_some()
        || parse_iso_date(s).is_some()
        || parse_iso_year_month(s).is_some()
        || parse_iso_month_day(s).is_some()
        || parse_temporal_time(s).is_some()
    {
        return Some("iso8601".to_string());
    }
    None
}

/// The rounding mode for the negated frame, used to implement `since` as
/// `negate(until)`: ceil/floor and halfCeil/halfFloor swap (they are direction-
/// sensitive); trunc/expand/halfTrunc/halfExpand/halfEven are sign-symmetric.
fn negate_mode(m: &str) -> String {
    match m {
        "ceil" => "floor",
        "floor" => "ceil",
        "halfCeil" => "halfFloor",
        "halfFloor" => "halfCeil",
        other => other,
    }
    .to_string()
}

/// Whether `s` is a well-formed UTC-offset string for a Temporal property-bag
/// `offset` field: a required sign, a 2-digit hour 00-23, then optional
/// minute / second components with a CONSISTENT separator style (all `:` or all
/// none), and an optional 1-9 digit sub-minute fraction. (Rejects "00:00" — no
/// sign, "+0" — short hour, "-000:00" — long hour, "+00:0000" — inconsistent.)
pub(crate) fn valid_offset_string(s: &str) -> bool {
    let b = s.as_bytes();
    let n = b.len();
    let two = |i: usize| -> Option<u32> {
        if i + 2 <= n && b[i].is_ascii_digit() && b[i + 1].is_ascii_digit() {
            Some((b[i] - b'0') as u32 * 10 + (b[i + 1] - b'0') as u32)
        } else {
            None
        }
    };
    if n == 0 || (b[0] != b'+' && b[0] != b'-') {
        return false;
    }
    // Hour: exactly two digits, 00-23.
    match two(1) {
        Some(h) if h <= 23 => {}
        _ => return false,
    }
    let mut i = 3;
    if i == n {
        return true;
    }
    let extended = b[i] == b':';
    // Minutes.
    if extended {
        i += 1;
    }
    match two(i) {
        Some(m) if m <= 59 => i += 2,
        _ => return false,
    }
    if i == n {
        return true;
    }
    // Seconds — the separator style must match the minutes'.
    if extended {
        if i >= n || b[i] != b':' {
            return false;
        }
        i += 1;
    }
    match two(i) {
        Some(sec) if sec <= 59 => i += 2,
        _ => return false,
    }
    if i == n {
        return true;
    }
    // Optional 1-9 digit sub-second fraction terminating the string.
    if b[i] == b'.' || b[i] == b',' {
        let frac = &b[i + 1..];
        return !frac.is_empty() && frac.len() <= 9 && frac.iter().all(|c| c.is_ascii_digit());
    }
    false
}

/// Parse a Temporal time-zone argument into a (normalized id, offset-ns) pair.
/// Stage 1: "UTC" and numeric offsets (±HH:MM[:SS]) carry a real offset; a named
/// IANA-style id ("Area/Location") is accepted with offset 0 (no tz database yet).
fn parse_time_zone(s: &str) -> Option<(String, i64)> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    // A full ISO string carrying a `[tz]` annotation: the time zone is the bracket
    // content (e.g. "2016-12-31T23:59:60+00:00[UTC]" -> "UTC", "…[+01:46]" -> the
    // offset zone). A leading `!` critical flag is stripped.
    if let Some(lb) = t.find('[') {
        let rb = t[lb..].find(']').map(|r| lb + r)?;
        let inner = &t[lb + 1..rb];
        let inner = inner.strip_prefix('!').unwrap_or(inner);
        // A non-empty prefix before the annotation means the whole string is a
        // "<datetime|offset>[tz]" form — the prefix is NOT decorative and must be a
        // valid ISO datetime/offset. Validate it (e.g. reject
        // "-000000-10-31T17:45+00:00[UTC]" for its negative-zero extended year),
        // not just the bracket content. A pure UTC offset (only digits/`:`/`.`/`,`
        // after the sign) is checked directly; anything else must parse as a full
        // zoned-datetime string (which validates the date via the hardened parser).
        let prefix = t[..lb].trim();
        if !prefix.is_empty() {
            let pure_offset = prefix.strip_prefix(['+', '-']).is_some_and(|r| {
                !r.is_empty()
                    && r.chars().all(|c| c.is_ascii_digit() || matches!(c, ':' | '.' | ','))
            });
            if pure_offset {
                if parse_offset_ns(prefix).is_none() {
                    return None;
                }
            } else if parse_zdt_string(t).is_none() {
                return None;
            }
        }
        return parse_time_zone(inner);
    }
    if t.eq_ignore_ascii_case("UTC") {
        return Some(("UTC".to_string(), 0));
    }
    let b = t.as_bytes();
    if b[0] == b'+' || b[0] == b'-' {
        let sign: i64 = if b[0] == b'-' { -1 } else { 1 };
        let body = &t[1..];
        // Both colon-separated (±HH, ±HH:MM, ±HH:MM:SS) and colon-less (±HH,
        // ±HHMM, ±HHMMSS) offset forms are valid time-zone identifiers. A
        // sub-minute/fractional offset is NOT a valid identifier (rejected).
        let (hh, mm, ss) = if body.contains(':') {
            let parts: Vec<&str> = body.split(':').collect();
            if parts.len() > 3 {
                return None;
            }
            let hh: i64 = parts.first()?.parse().ok()?;
            let mm: i64 = parts.get(1).map_or(Some(0), |p| p.parse().ok())?;
            let ss: i64 = parts.get(2).map_or(Some(0), |p| p.parse().ok())?;
            (hh, mm, ss)
        } else {
            if !body.bytes().all(|c| c.is_ascii_digit()) || !matches!(body.len(), 2 | 4 | 6) {
                return None;
            }
            let hh: i64 = body[0..2].parse().ok()?;
            let mm: i64 = if body.len() >= 4 { body[2..4].parse().ok()? } else { 0 };
            let ss: i64 = if body.len() >= 6 { body[4..6].parse().ok()? } else { 0 };
            (hh, mm, ss)
        };
        if hh > 23 || mm > 59 || ss > 59 {
            return None;
        }
        let off = sign * (hh * 3600 + mm * 60 + ss) * 1_000_000_000;
        return Some((t.to_string(), off));
    }
    // A bracket-less full ISO datetime string carries its own zone: "...Z" → UTC,
    // "...±HH:MM"/"...±HHMM" (minute precision) → that offset zone, normalized via
    // format_offset. A sub-minute offset or a bare datetime (no offset/Z) is NOT a
    // time-zone identifier. (Bracketed forms returned at the `[` branch above; bare
    // offset ids hit the `+`/`-` branch above.)
    if let Some(sep) = t.find(['T', 't', ' ']) {
        if parse_iso_date(&t[..sep]).is_some() {
            let tp = &t[sep + 1..];
            if let Some(z) = tp.find(['Z', 'z']) {
                return parse_iso_time(&tp[..z]).map(|_| ("UTC".to_string(), 0));
            }
            if let Some(o) = tp.find(['+', '-']) {
                let (time_str, off_str) = (&tp[..o], &tp[o..]);
                let after = &off_str[1..];
                // Minute precision only: a second ':' (=seconds) or a fraction is a
                // sub-minute offset, which is not a valid identifier.
                let minute_prec = after.matches(':').count() <= 1
                    && !after.contains('.')
                    && !after.contains(',')
                    && after.bytes().all(|c| c.is_ascii_digit() || c == b':');
                if parse_iso_time(time_str).is_some() && minute_prec {
                    if let Some(off) = parse_offset_ns(off_str) {
                        let off = off as i64;
                        return Some((format_offset(off), off));
                    }
                }
            }
            return None;
        }
    }
    // A named zone like "America/New_York" or "Europe/London": accept the id.
    if t.contains('/') || t.chars().all(|c| c.is_ascii_alphabetic() || c == '_') {
        return Some((t.to_string(), 0));
    }
    None
}

/// Add a Duration `f` ([y,mo,w,d,h,mi,s,ms,us,ns]) to a date-time `start`
/// ([y,mo,d,h,mi,s,ms,us,ns]) with calendar constrain — the shared add path.
fn dt_add_dur(cal: Cal, start: [i64; 9], f: [i64; 10]) -> [i64; 9] {
    let tns = time_to_ns(&[start[3], start[4], start[5], start[6], start[7], start[8]])
        + (f[4] as i128) * 3_600_000_000_000
        + (f[5] as i128) * 60_000_000_000
        + (f[6] as i128) * 1_000_000_000
        + (f[7] as i128) * 1_000_000
        + (f[8] as i128) * 1_000
        + (f[9] as i128);
    let carry = tns.div_euclid(DAY_NS) as i64;
    let nt = ns_to_time(tns.rem_euclid(DAY_NS));
    let (cy, cm, cd) = cal_from_iso(cal, start[0], start[1], start[2]);
    let (ay, am, ad) = cal_add_year_month(cal, cy, cm, cd, f[0], f[1], false).unwrap();
    let ed = cal_to_epoch_days(cal, ay, am, ad) + f[2] * 7 + f[3] + carry;
    let (ny, nm, nd) = epoch_days_to_iso(ed);
    [ny, nm, nd, nt[0], nt[1], nt[2], nt[3], nt[4], nt[5]]
}

/// Epoch nanoseconds of a date-time [y,mo,d,h,mi,s,ms,us,ns] (ISO/UTC frame).
fn dt_epoch_ns(dt: [i64; 9]) -> i128 {
    (iso_to_epoch_days(dt[0], dt[1], dt[2]) as i128) * DAY_NS
        + time_to_ns(&[dt[3], dt[4], dt[5], dt[6], dt[7], dt[8]])
}

/// Range-check the instant after adding duration `f` to a `relativeTo` anchor (the
/// target of Duration.prototype.total / round / compare). The result must be
/// representable: a ZonedDateTime anchor checks the true EPOCH of the target
/// (wall − the anchor's zone offset, inclusive ±nsMaxInstant) — the anchor itself
/// was validated when it was resolved; a Plain anchor uses ISODateTimeWithinLimits
/// on the target (inclusive ±(nsMaxInstant + nsPerDay) so the day-granular max
/// date anchor is accepted). `strict_plain_start` (round/total, NOT compare) also
/// requires a non-zero duration's plain START midnight to be STRICTLY within the
/// datetime bound: the minimum date -271821-04-19's midnight lands exactly ON the
/// exclusive ISODateTimeWithinLimits bound and must throw before any arithmetic.
fn check_relative_target(
    cal: Cal,
    start: [i64; 9],
    f: &[f64; 10],
    is_zoned: bool,
    offset_ns: i64,
    strict_plain_start: bool,
) -> Result<(), Thrown> {
    let end_ns = dur_end_epoch_ns(cal, start, f);
    let ok = if is_zoned {
        (end_ns - offset_ns as i128).abs() <= NS_MAX_INSTANT
    } else {
        let start_ok = !strict_plain_start
            || f.iter().all(|&x| x == 0.0)
            || dt_epoch_ns(start).abs() < NS_MAX_INSTANT + DAY_NS;
        start_ok && end_ns.abs() <= NS_MAX_INSTANT + DAY_NS
    };
    if !ok {
        return Err(Thrown("RangeError: Temporal result is outside the representable range".into()));
    }
    Ok(())
}

/// Exact i128 nanosecond total of the day+time portion of an f64 duration
/// record (integer-valued f64 → i128 truncation is exact; the max legal field,
/// ~9.007e24 ns, is far below 2^127).
fn dur_day_time_ns(f: &[f64; 10]) -> i128 {
    (f[3] as i128) * DAY_NS
        + (f[4] as i128) * 3_600_000_000_000
        + (f[5] as i128) * 60_000_000_000
        + (f[6] as i128) * 1_000_000_000
        + (f[7] as i128) * 1_000_000
        + (f[8] as i128) * 1_000
        + (f[9] as i128)
}

/// The epoch-ns of `start + duration` with EXACT day+time arithmetic: the
/// calendar part (y/mo/w, each below 2^32 by IsValidDuration so `as i64` is
/// exact) goes through date math, the day+time portion adds in i128 — a huge
/// sub-second field must not saturate through an i64 conversion.
fn dur_end_epoch_ns(cal: Cal, start: [i64; 9], f: &[f64; 10]) -> i128 {
    let cal_end =
        dt_add_dur(cal, start, [f[0] as i64, f[1] as i64, f[2] as i64, 0, 0, 0, 0, 0, 0, 0]);
    dt_epoch_ns(cal_end) + dur_day_time_ns(f)
}

/// `nsMaxInstant` — the inclusive epoch-nanosecond bound of `Temporal.Instant`
/// (±8.64 × 10^21, i.e. ±10^8 days).
pub(crate) const NS_MAX_INSTANT: i128 = 8_640_000_000_000_000_000_000;

/// ISODateTimeWithinLimits: whether the date-time `f` is a representable
/// `Temporal.PlainDateTime` — its epoch-ns must lie strictly within `nsMaxInstant`
/// plus a one-day (`nsPerDay`) margin on each side. (Unlike `PlainDate`'s
/// day-granular bound, this is nanosecond-precise: `-271821-04-19T00:00:00.000000000`
/// is out of range but `…000000001` is in range.) Caller must have already
/// validated the field ranges so `dt_epoch_ns` cannot overflow.
pub(crate) fn iso_datetime_ns_in_range(f: [i64; 9]) -> bool {
    let ns = dt_epoch_ns(f);
    ns > -NS_MAX_INSTANT - DAY_NS && ns < NS_MAX_INSTANT + DAY_NS
}

/// Round a date-time difference `df` whose smallestUnit is a DAY-or-time unit while
/// largestUnit is a calendar unit (year/month/week): round the day+time remainder to
/// the smallestUnit increment, then re-balance to `largest` so a rounded-up day rolls
/// into the calendar units. The `rounded == time_ns` short-circuit keeps the
/// nanosecond-default case (and difference_datetime's day-borrow) byte-identical.
fn round_datetime_diff_daytime(
    cal: Cal,
    dt1: [i64; 9],
    df: [i64; 10],
    smallest: &str,
    largest: &str,
    inc: i128,
    mode: &str,
) -> [i64; 10] {
    let time_ns = (df[3] as i128) * DAY_NS
        + time_to_ns(&[df[4], df[5], df[6], df[7], df[8], df[9]]);
    let rounded = round_increment(time_ns, unit_ns(smallest) * inc, mode);
    if rounded == time_ns {
        return df;
    }
    // Reconstruct the rounded endpoint (anchor at the kept year/month/week units, then
    // add the rounded day+time span) and re-decompose at largestUnit.
    let anchor = dt_add_dur(cal, dt1, [df[0], df[1], df[2], 0, 0, 0, 0, 0, 0, 0]);
    let total = dt_epoch_ns(anchor) + rounded;
    let (ey, em, ed) = epoch_days_to_iso(total.div_euclid(DAY_NS) as i64);
    let t = ns_to_time(total.rem_euclid(DAY_NS));
    let end = [ey, em, ed, t[0], t[1], t[2], t[3], t[4], t[5]];
    difference_datetime_cal(cal, dt1, end, largest)
}

/// Round the date-time difference dt1→dt2 to a calendar `smallest` unit
/// (year/month/week), then balance to `largest`. Like round_relative_date_diff
/// but the fraction toward the next unit is measured in epoch NANOSECONDS, so
/// the time-of-day contributes (NudgeToCalendarUnit for PlainDateTime/ZDT).
fn round_relative_datetime_diff(
    cal: Cal,
    dt1: [i64; 9],
    dt2: [i64; 9],
    smallest: &str,
    largest: &str,
    inc: i128,
    mode: &str,
) -> Result<[i64; 10], Thrown> {
    let si = ["year", "month", "week"].iter().position(|&x| x == smallest).unwrap_or(2);
    let ns1 = dt_epoch_ns(dt1);
    let ns2 = dt_epoch_ns(dt2);
    let sign = (ns2 > ns1) as i64 - (ns2 < ns1) as i64;
    if sign == 0 {
        return Ok([0; 10]);
    }
    // Decompose at largestUnit to KEEP the units above smallestUnit; only the
    // smallestUnit component is rounded (the sub-smallest remainder, including the
    // time-of-day, becomes the epoch-ns fraction toward the next increment).
    let base = difference_datetime_cal(cal, dt1, dt2, largest);
    // smallestUnit = week: difference dumps the sub-month remainder into days, so
    // derive the whole-week count from the full sub-week day span.
    let sval = if si == 2 { (base[2] * 7 + base[3]) / 7 } else { base[si] };
    let mk = |k: i64| -> [i64; 10] {
        let mut d = [0i64; 10];
        d[..si].copy_from_slice(&base[..si]);
        d[si] = k;
        d
    };
    let mut r1 = round_increment(sval as i128, inc, "trunc") as i64;
    // NudgeToCalendarUnit brackets the target between r1 and r1+increment. The
    // difference above can UNDER-count when the anchor is at the end of a month
    // (2023-05-31 → 2024-04-30 is 10 months + 30 days, yet 11 whole months from
    // the anchor land exactly on it), so advance while the next increment still
    // does not pass the target.
    loop {
        let nxt = r1 + inc as i64 * sign;
        let e = dt_epoch_ns(dt_add_dur(cal, dt1, mk(nxt)));
        if (sign > 0 && e <= ns2) || (sign < 0 && e >= ns2) {
            r1 = nxt;
        } else {
            break;
        }
    }
    let r2 = r1 + inc as i64 * sign;
    let lower = dt_add_dur(cal, dt1, mk(r1));
    let ld = dt_epoch_ns(lower);
    // The r2 endpoint is a CalendarDateAdd(constrain) that must lie within the ISO
    // date limits — a huge increment can push it past the range (RangeError).
    let upper = dt_add_dur(cal, dt1, mk(r2));
    if !iso_date_in_range(upper[0], upper[1], upper[2]) {
        return Err(Thrown(
            "RangeError: rounded date is outside the valid ISO range".into(),
        ));
    }
    let picked = if ld == ns2 {
        r1
    } else {
        let ud = dt_epoch_ns(upper);
        let progress = if ud != ld { (ns2 - ld) as f64 / (ud - ld) as f64 } else { 0.0 };
        round_fraction(r1 / inc as i64, sign, progress, mode) * inc as i64
    };
    // Weeks never fold into a larger calendar unit: keep years/months + rounded weeks.
    if si == 2 {
        return Ok([base[0], base[1], picked, 0, 0, 0, 0, 0, 0, 0]);
    }
    // BubbleRelativeDuration: the result is the kept larger units plus the rounded
    // smallest unit; only a smallest unit that reached a whole larger one folds up.
    // (Re-differencing the endpoint would re-introduce the end-of-month under-count.)
    let mut f = mk(picked);
    if si == 1 && largest == "year" {
        let miy = cal_months_in_year(cal, dt1[0]);
        f[0] += f[1] / miy;
        f[1] %= miy;
    }
    Ok(f)
}

/// `Duration.total(unit)` relative to a start date-time: the (possibly fractional)
/// total of the duration measured in `unit`, computed via the calendar at `start`.
fn duration_total_relative(
    cal: Cal,
    f: [i64; 10],
    start: [i64; 9],
    unit: &str,
) -> Result<f64, Thrown> {
    let end_ns = dt_epoch_ns(dt_add_dur(cal, start, f));
    let start_ns = dt_epoch_ns(start);
    let diff = end_ns - start_ns;
    match unit {
        "year" | "month" => {
            let sign = if diff < 0 { -1i64 } else { 1 };
            let units = |k: i64| -> [i64; 10] {
                let mut d = [0i64; 10];
                if unit == "year" {
                    d[0] = k;
                } else {
                    d[1] = k;
                }
                d
            };
            // Whole signed units, ALWAYS re-added from the anchor (chaining would let
            // day-of-month clamping accumulate and corrupt the unit length).
            let mut whole = 0i64;
            for _ in 0..2_000_000 {
                let cand = dt_epoch_ns(dt_add_dur(cal, start, units(whole + sign)));
                if (sign > 0 && cand > end_ns) || (sign < 0 && cand < end_ns) {
                    break;
                }
                whole += sign;
            }
            // NudgeToCalendarUnit brackets the duration between `whole` and `whole+sign`
            // calendar units from the anchor (both via CalendarDateAdd). The FAR bracket
            // (past the duration's end) is the one that can exceed the date range; it
            // must be representable (inclusive ±(nsMaxInstant+nsPerDay), so the
            // day-granular min/max date boundary itself is still accepted).
            let far = dt_add_dur(cal, start, units(whole + sign));
            if dt_epoch_ns(far).abs() > NS_MAX_INSTANT + DAY_NS {
                return Err(Thrown(
                    "RangeError: Temporal result is outside the representable range".into(),
                ));
            }
            // The fraction is the signed progress over the anchor-based unit length.
            let lower_ns = dt_epoch_ns(dt_add_dur(cal, start, units(whole)));
            let upper_ns = dt_epoch_ns(far);
            if upper_ns != lower_ns {
                // whole + sign·(end-lower)/(upper-lower) as one correctly-rounded
                // rational, so the final addition can't round the wrong way.
                let span = upper_ns - lower_ns;
                Ok(rational_to_f64(
                    whole as i128 * span + sign as i128 * (end_ns - lower_ns),
                    span,
                ))
            } else {
                Ok(whole as f64)
            }
        }
        "week" => {
            // One correctly-rounded division of the exact total (the naive
            // whole+remainder split is still 1 ULP off in some cases).
            Ok(rational_to_f64(diff, 7 * DAY_NS))
        }
        _ => {
            // Same for a fixed-length unit (day/hour/…/ns).
            Ok(rational_to_f64(diff, unit_ns(unit)))
        }
    }
}

/// Parse a ZonedDateTime ISO string `<date>[T<time>][±OFF|Z][tzid][annotations]`
/// into (date-time fields, offset ns, tz id). The `[tzid]` annotation is REQUIRED
/// (it carries the zone); a leading `!` critical flag is stripped. The explicit
/// numeric offset / `Z` is OPTIONAL — when absent the offset comes from the zone
/// (so `1970-01-01T00:00[UTC]` and `2020-01-01[+09:00]` parse). The time part is
/// optional (date-only -> midnight). Basic-format offsets (`-0800`) are accepted.
/// Parse a ZonedDateTime/relativeTo string. Returns the wall-clock fields, the
/// instant offset (ns), the zone id, the zone's offset (ns), and the *offset
/// behaviour*: `0` = WALL (no explicit offset → use the zone), `1` = EXACT (a `Z`
/// designator → the instant is UTC, the offset is not reconciled), `2` = OPTION
/// (an explicit `±HH:MM` offset → reconcile against the zone per the `offset`
/// option). The behaviour drives ToTemporalZonedDateTime's InterpretISODateTimeOffset.
fn parse_zdt_string(s: &str) -> Option<([i64; 9], i64, String, i64, i8)> {
    let lb = s.find('[')?;
    let rb = s[lb..].find(']').map(|r| lb + r)?;
    let mut tz = s[lb + 1..rb].to_string();
    if let Some(stripped) = tz.strip_prefix('!') {
        tz = stripped.to_string();
    }
    // The bracket must be a valid time zone (named or numeric-offset); its offset
    // is the fallback when the datetime carries no explicit offset.
    let (tz_id, tz_offset) = parse_time_zone(&tz)?;
    let head = &s[..lb];
    // Split off an optional time part at the date/time separator.
    let (date_part, time_part) = match head.find(['T', 't', ' ']) {
        Some(tp) => (&head[..tp], Some(&head[tp + 1..])),
        None => (head, None),
    };
    let date = parse_iso_date(date_part)?;
    // In the time part, locate an explicit `Z` or numeric offset (else use the
    // zone's offset). The time itself never contains `+`/`-`, so the first one
    // begins the offset.
    let (time_str, offset_ns, behaviour) = match time_part {
        None => ("", tz_offset, 0i8),
        Some(t) => {
            if let Some(zpos) = t.find(['Z', 'z']) {
                (&t[..zpos], 0i64, 1i8)
            } else if let Some(opos) = t.find(['+', '-']) {
                // The offset must satisfy the strict UTC-offset grammar (2-digit
                // hour, consistent ':'/no-':' groups) — parse_offset_ns alone
                // strips separators and accepted mixed forms like '+00:0000'.
                let off_str = &t[opos..];
                if !valid_offset_string(off_str) {
                    return None;
                }
                (&t[..opos], parse_offset_ns(off_str)? as i64, 2i8)
            } else {
                (t, tz_offset, 0i8)
            }
        }
    };
    let time = if time_str.is_empty() { [0i64; 6] } else { parse_iso_time(time_str)? };
    let f = [date.0, date.1, date.2, time[0], time[1], time[2], time[3], time[4], time[5]];
    // Return the zone's offset alongside the (possibly explicit) string offset so the
    // caller can reconcile a mismatch per the `offset` option / offset behaviour.
    Some((f, offset_ns, tz_id, tz_offset, behaviour))
}

/// DifferenceISODateTime measured in a calendar: identical to
/// `difference_datetime` except the date part is decomposed with the calendar's
/// years/months. (A largestUnit below "day" never reaches the date split, so
/// those cases route straight through.)
fn difference_datetime_cal(cal: Cal, dt1: [i64; 9], dt2: [i64; 9], largest: &str) -> [i64; 10] {
    if cal == Cal::Iso
        || matches!(
            largest,
            "hour" | "minute" | "second" | "millisecond" | "microsecond" | "nanosecond"
        )
    {
        return difference_datetime(dt1, dt2, largest);
    }
    let time1 = time_to_ns(&[dt1[3], dt1[4], dt1[5], dt1[6], dt1[7], dt1[8]]);
    let time2 = time_to_ns(&[dt2[3], dt2[4], dt2[5], dt2[6], dt2[7], dt2[8]]);
    let mut time_diff = time2 - time1;
    let date1 = (dt1[0], dt1[1], dt1[2]);
    let e1 = iso_to_epoch_days(date1.0, date1.1, date1.2);
    let e2 = iso_to_epoch_days(dt2[0], dt2[1], dt2[2]);
    let date_sign = (e2 > e1) as i64 - (e2 < e1) as i64;
    let mut date2 = (dt2[0], dt2[1], dt2[2]);
    // Borrow a day when the time part runs opposite to the date direction.
    if time_diff != 0 && date_sign != 0 && time_diff.signum() != date_sign as i128 {
        date2 = epoch_days_to_iso(iso_to_epoch_days(date2.0, date2.1, date2.2) - date_sign);
        time_diff += (date_sign as i128) * DAY_NS;
    }
    let mut df = [0i64; 10];
    df[..4].copy_from_slice(&cal_difference_date(cal, date1, date2, largest));
    let tsign = time_diff.signum() as i64;
    let t = ns_to_time(time_diff.abs());
    for i in 0..6 {
        df[4 + i] = t[i] * tsign;
    }
    df
}

/// Whether a Temporal ISO string carries a FULL date (year-month-day), as
/// opposed to the dateless `YYYY-MM` / `MM-DD` shorthands. Only a full date can
/// be reinterpreted in a non-ISO calendar, so this gates the calendar
/// annotation on the PlainYearMonth/PlainMonthDay string paths.
fn temporal_string_has_date(s: &str) -> bool {
    temporal_string_date(s).is_some()
}

/// The full (year, month, day) of a Temporal ISO string, or `None` for the
/// dateless `YYYY-MM` / `MM-DD` shorthands.
fn temporal_string_date(s: &str) -> Option<(i64, i64, i64)> {
    let main = s.trim().split('[').next().unwrap_or("");
    let date_part = main.split(['T', 't', ' ']).next().unwrap_or("");
    parse_iso_date(date_part)
}

/// Format a UTC offset (nanoseconds) as `±HH:MM` (or `±HH:MM:SS` when needed).
fn format_offset(ns: i64) -> String {
    let sign = if ns < 0 { '-' } else { '+' };
    let total = ns.abs() / 1_000_000_000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if s == 0 {
        format!("{sign}{h:02}:{m:02}")
    } else {
        format!("{sign}{h:02}:{m:02}:{s:02}")
    }
}

// submodules (split out of the former monolithic temporal.rs)
mod calendar;
pub(crate) use calendar::*;
mod duration;
mod plain_date;
mod plain_time;
mod plain_date_time;
mod instant_zdt;
mod year_month_day;
