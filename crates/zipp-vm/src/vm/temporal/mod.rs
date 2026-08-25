#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
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
            let key_char =
                |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_';
            let valid_key = key
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
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
                    || (!off.contains(':')
                        && off.bytes().filter(|c| c.is_ascii_digit()).count() > 4);
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
            let n = chars[i + 1..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count();
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
                    && r.chars()
                        .all(|c| c.is_ascii_digit() || matches!(c, ':' | '.' | ','))
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
            let mm: i64 = if body.len() >= 4 {
                body[2..4].parse().ok()?
            } else {
                0
            };
            let ss: i64 = if body.len() >= 6 {
                body[4..6].parse().ok()?
            } else {
                0
            };
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
    // A named zone: GetAvailableNamedTimeZoneIdentifier against the bundled
    // IANA table (see `tzdb.rs`). The match is ASCII-case-insensitive and the
    // CANONICAL spelling is what comes back, so "africa/cairo" becomes
    // "Africa/Cairo"; a name the database does not have returns None, which
    // every caller turns into a RangeError. The offset is 0 here because a
    // named zone does not have one — it has a function of the instant, which
    // is `tz_offset_ns_at`.
    tzdb::lookup(t).map(|z| (z.canonical.to_string(), 0))
}

/// The UTC offset (nanoseconds) a time-zone IDENTIFIER has at an instant:
/// fixed for an offset zone, and GetNamedTimeZoneOffsetNanoseconds against the
/// IANA table for a named one. An unrecognised id yields 0 — identifiers are
/// validated where they enter (`parse_time_zone`), so this is unreachable for
/// a stored zone.
pub(crate) fn tz_offset_ns_at(id: &str, epoch_ns: i128) -> i64 {
    if id.starts_with(['+', '-']) {
        return parse_offset_ns(id).unwrap_or(0) as i64;
    }
    match tzdb::lookup(id) {
        // FLOOR to seconds: transitions land on whole seconds, so the offset
        // for a negative sub-second instant is the one of the second it is in.
        Some(z) => {
            tzdb::offset_seconds(z.zone, epoch_ns.div_euclid(1_000_000_000) as i64) as i64
                * 1_000_000_000
        }
        None => 0,
    }
}

/// GetStartOfDay: the first instant of a local calendar day. Normally that is
/// local midnight, but when a spring-forward SKIPS midnight the day begins at
/// the transition itself — which is not the same as disambiguating midnight
/// "compatible". America/Toronto on 1919-03-31 jumped 23:30 → 00:30, so its day
/// starts at 00:30, while disambiguated midnight lands at 01:00.
pub(crate) fn tz_start_of_day(id: &str, local_midnight_ns: i128) -> Result<i128, Thrown> {
    if id.starts_with(['+', '-']) {
        return Ok(local_midnight_ns - parse_offset_ns(id).unwrap_or(0) as i128);
    }
    let zone = match tzdb::lookup(id) {
        Some(z) => z.zone,
        None => return Ok(local_midnight_ns),
    };
    let (sec, rem) = (
        local_midnight_ns.div_euclid(1_000_000_000),
        local_midnight_ns.rem_euclid(1_000_000_000),
    );
    if let Some(&first) = tzdb::possible_instants(zone, sec as i64).first() {
        return Ok(first as i128 * 1_000_000_000 + rem);
    }
    match tzdb::next_transition(zone, sec as i64 - 86_400) {
        Some(t) => Ok(t as i128 * 1_000_000_000),
        None => tz_local_to_instant(id, local_midnight_ns, "compatible"),
    }
}

/// GetNamedTimeZoneNextTransition / …PreviousTransition, in nanoseconds. An
/// offset zone never has one. The database works in whole seconds, so a
/// sub-second instant is floored for "next" and ceilinged for "previous" —
/// otherwise `getTimeZoneTransition("next")` called ON a transition with a
/// nanosecond to spare would return that same transition again.
pub(crate) fn tz_transition(id: &str, epoch_ns: i128, next: bool) -> Option<i128> {
    if id.starts_with(['+', '-']) {
        return None;
    }
    let z = tzdb::lookup(id)?;
    let sec = if next {
        epoch_ns.div_euclid(1_000_000_000) as i64
    } else {
        // A transition strictly BEFORE a sub-second instant may be in the same
        // second, so round up before searching.
        (-((-epoch_ns).div_euclid(1_000_000_000))) as i64
    };
    let t = if next {
        tzdb::next_transition(z.zone, sec)
    } else {
        tzdb::previous_transition(z.zone, sec)
    }?;
    let ns = t as i128 * 1_000_000_000;
    // The annual rules recur forever, but a transition outside the representable
    // instant range is not one Temporal can name: `getTimeZoneTransition("next")`
    // on the maximum instant must be null, not a RangeError.
    (ns.abs() <= NS_MAX_INSTANT).then_some(ns)
}

/// InterpretISODateTimeOffset: turn a wall clock plus an (optional) explicit
/// offset into an instant.
///
/// `behaviour` is how the offset was supplied — 0 WALL (none given), 1 EXACT (a
/// `Z` designator, which fixes the instant outright), 2 OPTION (an explicit
/// numeric offset that has to be reconciled with the zone). `match_minutes` is
/// the MATCH-MINUTES rule for strings: an offset written to minute precision
/// still matches a zone whose real offset has seconds in it (every pre-1900
/// LMT offset does).
pub(crate) fn interpret_iso_offset(
    id: &str,
    local_ns: i128,
    behaviour: u8,
    offset_ns: i64,
    disambiguation: &str,
    offset_option: &str,
    match_minutes: bool,
) -> Result<i128, Thrown> {
    if behaviour == 0 || offset_option == "ignore" {
        return tz_local_to_instant(id, local_ns, disambiguation);
    }
    if behaviour == 1 || offset_option == "use" {
        return Ok(local_ns - offset_ns as i128);
    }
    // OPTION with "prefer"/"reject": an instant whose real offset IS the one
    // written wins outright; only when none matches does the option decide
    // between throwing and falling back to the zone's own answer.
    if !id.starts_with(['+', '-']) {
        if let Some(z) = tzdb::lookup(id) {
            let (sec, rem) = (
                local_ns.div_euclid(1_000_000_000),
                local_ns.rem_euclid(1_000_000_000),
            );
            for c in tzdb::possible_instants(z.zone, sec as i64) {
                let cand = c as i128 * 1_000_000_000 + rem;
                let off = tzdb::offset_seconds(z.zone, c) as i64 * 1_000_000_000;
                let rounded = {
                    let m = 60_000_000_000i64;
                    // RoundNumberToIncrement(off, 60e9, "halfExpand").
                    let (q, r) = (off / m, off % m);
                    if r.abs() * 2 >= m {
                        (q + if off < 0 { -1 } else { 1 }) * m
                    } else {
                        q * m
                    }
                };
                if off == offset_ns || (match_minutes && rounded == offset_ns) {
                    return Ok(cand);
                }
            }
        }
    } else if parse_offset_ns(id).unwrap_or(0) as i64 == offset_ns {
        return Ok(local_ns - offset_ns as i128);
    }
    if offset_option == "reject" {
        return Err(Thrown(
            "RangeError: the offset does not match the time zone".into(),
        ));
    }
    tz_local_to_instant(id, local_ns, disambiguation)
}

/// GetPossibleEpochNanoseconds + DisambiguatePossibleEpochNanoseconds: the
/// instant a local wall clock denotes in a zone. A spring-forward gap has no
/// instant and a fall-back repeat has two, which is what `disambiguation`
/// resolves ("reject" throws in either case).
pub(crate) fn tz_local_to_instant(
    id: &str,
    local_ns: i128,
    disambiguation: &str,
) -> Result<i128, Thrown> {
    if id.starts_with(['+', '-']) {
        return Ok(local_ns - parse_offset_ns(id).unwrap_or(0) as i128);
    }
    let zone = match tzdb::lookup(id) {
        Some(z) => z.zone,
        None => return Ok(local_ns),
    };
    // Offsets are whole seconds, so the sub-second part of the wall clock rides
    // along untouched and only the second-granular part needs the database.
    let (sec, rem) = (
        local_ns.div_euclid(1_000_000_000),
        local_ns.rem_euclid(1_000_000_000),
    );
    let cands = tzdb::possible_instants(zone, sec as i64);
    let pick = |v: &[i64], last: bool| -> i128 {
        (if last { v[v.len() - 1] } else { v[0] }) as i128 * 1_000_000_000 + rem
    };
    match cands.len() {
        1 => Ok(pick(&cands, false)),
        n if n >= 2 => match disambiguation {
            "reject" => Err(Thrown(
                "RangeError: this wall-clock time occurs twice in this time zone".into(),
            )),
            "later" => Ok(pick(&cands, true)),
            // "compatible" and "earlier" both take the first instant.
            _ => Ok(pick(&cands, false)),
        },
        // A gap. Shift the wall clock by the size of the gap — the difference
        // between the offsets a day either side — and take the instant on the
        // far side of it: forwards for "compatible"/"later", backwards for
        // "earlier".
        _ => {
            if disambiguation == "reject" {
                return Err(Thrown(
                    "RangeError: this wall-clock time does not exist in this time zone".into(),
                ));
            }
            let before = tzdb::offset_seconds(zone, sec as i64 - 86_400) as i64;
            let after = tzdb::offset_seconds(zone, sec as i64 + 86_400) as i64;
            let shift = after - before;
            let earlier = disambiguation == "earlier";
            let shifted = sec as i64 + if earlier { -shift } else { shift };
            let v = tzdb::possible_instants(zone, shifted);
            if v.is_empty() {
                // Cannot happen for a real zone (the shift is exactly the gap),
                // but a bad table must not panic.
                return Ok(local_ns - before as i128 * 1_000_000_000);
            }
            Ok(pick(&v, earlier))
        }
    }
}

// ── the zoned duration machinery ───────────────────────────────────────────
//
// A day in a real time zone is 23, 24 or 25 hours long (23.5 in Lord Howe, and
// 23.5 again in 1919 Toronto, which sprang forward at 23:30). So a duration's
// DATE part has to move on the WALL CLOCK and be re-zoned, while its TIME part
// stays exact elapsed nanoseconds — and every "how far between here and there"
// question has to measure the bracket on the timeline rather than assume 86400
// seconds. Everything below follows the specification's zoned algorithms
// (AddZonedDateTime, DifferenceZonedDateTime, NudgeTo*, BubbleRelativeDuration,
// RoundRelativeDuration, TotalRelativeDuration) literally. `tz` may be an
// offset zone, which simply makes every day exactly 24 hours again.

/// GetISODateTimeFor: the wall clock `tz` shows at an instant.
fn zoned_wall(tz: &str, ns: i128) -> [i64; 9] {
    let local = ns + tz_offset_ns_at(tz, ns) as i128;
    let (y, mo, d) = epoch_days_to_iso(local.div_euclid(DAY_NS) as i64);
    let t = ns_to_time(local.rem_euclid(DAY_NS));
    [y, mo, d, t[0], t[1], t[2], t[3], t[4], t[5]]
}

/// GetEpochNanosecondsFor(tz, dt, COMPATIBLE), with the CheckISODaysRange and
/// IsValidEpochNanoseconds guards that plain `tz_local_to_instant` leaves to its
/// callers: the bracket endpoints this machinery materializes are exactly where
/// an out-of-range instant surfaces, and each must be a RangeError.
fn zoned_epoch(tz: &str, dt: [i64; 9]) -> Result<i128, Thrown> {
    let oor =
        || Thrown("RangeError: Temporal result is outside the representable range".to_string());
    if iso_to_epoch_days(dt[0], dt[1], dt[2]).abs() > 100_000_000 {
        return Err(oor());
    }
    let ns = tz_local_to_instant(tz, dt_epoch_ns(dt), "compatible")?;
    if ns.abs() > NS_MAX_INSTANT {
        return Err(oor());
    }
    Ok(ns)
}

/// The hours-and-below part of a duration in nanoseconds, exactly. DAYS are
/// excluded on purpose — in a zone a day is not 24 hours.
fn dur_time_only_ns(f: &[f64; 10]) -> i128 {
    (f[4] as i128) * 3_600_000_000_000
        + (f[5] as i128) * 60_000_000_000
        + (f[6] as i128) * 1_000_000_000
        + (f[7] as i128) * 1_000_000
        + (f[8] as i128) * 1_000
        + (f[9] as i128)
}

/// AddZonedDateTime: the y/mo/w/d part moves on the wall clock and is re-zoned
/// with "compatible" disambiguation; the h..ns part is then exact elapsed time.
/// "+1 day" across a spring-forward is 23 real hours; "+24 hours" is 24.
pub(crate) fn add_zoned(cal: Cal, tz: &str, ns: i128, f: &[f64; 10]) -> Result<i128, Thrown> {
    let oor =
        || Thrown("RangeError: Temporal result is outside the representable range".to_string());
    let time_ns = dur_time_only_ns(f);
    let out = if f[..4].iter().all(|&x| x == 0.0) {
        ns + time_ns
    } else {
        let moved = dt_add_dur(
            cal,
            zoned_wall(tz, ns),
            [
                f[0] as i64,
                f[1] as i64,
                f[2] as i64,
                f[3] as i64,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
        );
        if !iso_datetime_ns_in_range(moved) {
            return Err(oor());
        }
        zoned_epoch(tz, moved)? + time_ns
    };
    if out.abs() > NS_MAX_INSTANT {
        return Err(oor());
    }
    Ok(out)
}

/// The instant a DATE-only duration reaches from `origin`, via AddZonedDateTime.
///
/// Every bracket and intermediate point in this machinery goes through here
/// rather than re-materializing a wall clock, because AddZonedDateTime is the
/// IDENTITY for a zero date duration. An origin inside the SECOND occurrence of
/// a repeated hour has a wall clock whose "compatible" disambiguation is the
/// FIRST occurrence, so re-materializing it silently moves the anchor an hour
/// and measures a 25-hour bracket where the real one is 24
/// (tc39/proposal-temporal#3148, and #3141 for the same effect in
/// DifferenceZonedDateTime).
fn zoned_step(cal: Cal, tz: &str, origin: i128, d: [i64; 4]) -> Result<i128, Thrown> {
    let mut f = [0f64; 10];
    for i in 0..4 {
        f[i] = d[i] as f64;
    }
    add_zoned(cal, tz, origin, &f)
}

/// DifferenceZonedDateTime: the date part is measured on the wall clock in `tz`,
/// the remainder is exact elapsed time. The day-correction loop is the
/// specification's — a DST shift can leave that remainder pointing backwards
/// (2000-04-02T01:30 → 04:30 in Vancouver is 2 hours, not 1 day minus 22), and
/// the intermediate date then steps one day back toward the start until it does
/// not. Whatever it settles on is exact by construction: the intermediate
/// instant plus the remainder IS ns2. Returns
/// ([years, months, weeks, days], remaining nanoseconds).
fn difference_zoned(
    cal: Cal,
    tz: &str,
    ns1: i128,
    ns2: i128,
    largest: &str,
) -> Result<([i64; 4], i128), Thrown> {
    if ns1 == ns2 {
        return Ok(([0; 4], 0));
    }
    let s = zoned_wall(tz, ns1);
    let e = zoned_wall(tz, ns2);
    let sign: i64 = if ns2 > ns1 { 1 } else { -1 };
    // Going forward a spring-forward can need two days of correction; going
    // back, one.
    let max_corr = if sign > 0 { 2 } else { 1 };
    let s_days = iso_to_epoch_days(s[0], s[1], s[2]);
    let e_days = iso_to_epoch_days(e[0], e[1], e[2]);
    let st = time_to_ns(&[s[3], s[4], s[5], s[6], s[7], s[8]]);
    let et = time_to_ns(&[e[3], e[4], e[5], e[6], e[7], e[8]]);
    // An end wall TIME on the far side of the start's, relative to the direction
    // of travel, means the whole-day part has overshot: 2000-05-02T02:00 back to
    // 2000-04-02T03:00 in Vancouver is −29 days −23 hours, not −1 month (the
    // uncorrected −30 days lands on 02:00, an hour that does not exist, and
    // disambiguating it forward hides the overshoot).
    let mut first = if (et - st).signum() as i64 == -sign {
        1
    } else {
        0
    };
    // …unless correcting would push the DATE part backwards, which happens when
    // the start's own wall clock is the SECOND occurrence of a repeated hour: the
    // clocks run backwards there while the instants run forwards, and the
    // uncorrected step — which AddZonedDateTime resolves as the identity, keeping
    // the anchor on its own side of the fold — is the right one (#3141).
    if first == 1 && (e_days - sign - s_days).signum() as i64 == -sign {
        first = 0;
    }
    let mut fallback = None;
    let mut chosen = None;
    for corr in first..=max_corr {
        let idays = e_days - corr * sign;
        let inter = epoch_days_to_iso(idays);
        // A candidate that leaves the representable instant range is skipped,
        // not raised: both endpoints ARE representable, so such a candidate lies
        // beyond ns2 and the sign test below would reject it anyway. Letting it
        // throw turned `instance.until(limit, {largestUnit:"years"})` — an
        // ordinary difference — into a RangeError.
        let Ok(ins) = zoned_step(cal, tz, ns1, [0, 0, 0, idays - s_days]) else {
            continue;
        };
        let t = ns2 - ins;
        fallback = Some((inter, t));
        if t.signum() as i64 != -sign {
            chosen = Some((inter, t));
            break;
        }
    }
    let (inter, time_ns) = chosen.or(fallback).unwrap_or(((e[0], e[1], e[2]), 0));
    let date_largest = if matches!(largest, "year" | "month" | "week") {
        largest
    } else {
        "day"
    };
    Ok((
        cal_difference_date(cal, (s[0], s[1], s[2]), inter, date_largest),
        time_ns,
    ))
}

/// The outcome of a nudge: the rounded date part, the rounded time remainder,
/// the instant the rounded duration lands on, and whether it expanded to the
/// away-from-zero bracket (which is what BubbleRelativeDuration keys on).
struct Nudge {
    date: [i64; 4],
    time_ns: i128,
    nudged_ns: i128,
    expanded: bool,
}

/// round_fraction with the fraction given EXACTLY as `num`/`den` (both
/// magnitudes, `num` ≤ `den`) rather than as an f64. The half of a 25-hour day
/// is 45000000000000 out of 90000000000000 nanoseconds; an f64 quotient cannot
/// be trusted to land on the right side of a tie at that magnitude.
pub(crate) fn round_fraction_exact(lower: i64, sign: i64, num: i128, den: i128, mode: &str) -> i64 {
    if num == 0 {
        return lower;
    }
    let upper = lower + sign;
    let pick_upper = match mode {
        "ceil" => sign > 0,
        "floor" => sign < 0,
        "trunc" => false,
        "expand" => true,
        _ => {
            let twice = 2 * num;
            if twice > den {
                true
            } else if twice < den {
                false
            } else {
                match mode {
                    "halfCeil" => sign > 0,
                    "halfFloor" => sign < 0,
                    "halfTrunc" => false,
                    "halfEven" => upper.rem_euclid(2) == 0,
                    _ => true, // halfExpand (default)
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

/// NudgeToCalendarUnit: bracket the duration between the two whole multiples of
/// `unit` around it (r1 toward zero, r2 away), materialize both brackets as
/// instants in the zone, and choose per `mode` from the exact fraction between
/// them. That fraction is what makes a 25-hour day count as 25 hours. Returns
/// the nudge plus [[Total]] — r1 plus the fraction, which is what
/// `Duration.prototype.total` reports for a calendar unit (and for "day" in a
/// zone).
fn nudge_calendar(
    cal: Cal,
    tz: &str,
    origin: i128,
    date: [i64; 4],
    dest_ns: i128,
    sign: i64,
    inc: i64,
    unit: &str,
    mode: &str,
) -> Result<(Nudge, f64), Thrown> {
    let start = zoned_wall(tz, origin);
    let step = inc * sign;
    let (r1, sd, ed): (i64, [i64; 4], [i64; 4]) = match unit {
        "year" => {
            let k = (date[0] / inc) * inc;
            (k, [k, 0, 0, 0], [k + step, 0, 0, 0])
        }
        "month" => {
            let k = (date[1] / inc) * inc;
            (k, [date[0], k, 0, 0], [date[0], k + step, 0, 0])
        }
        "week" => {
            // Weeks are counted from the years+months anchor, so the days part
            // of the duration has to be re-expressed as whole weeks first.
            let ym = dt_add_dur(cal, start, [date[0], date[1], 0, 0, 0, 0, 0, 0, 0, 0]);
            let we = epoch_days_to_iso(iso_to_epoch_days(ym[0], ym[1], ym[2]) + date[3]);
            let u = cal_difference_date(cal, (ym[0], ym[1], ym[2]), we, "week");
            let k = ((date[2] + u[2]) / inc) * inc;
            (k, [date[0], date[1], k, 0], [date[0], date[1], k + step, 0])
        }
        _ => {
            let k = (date[3] / inc) * inc;
            (
                k,
                [date[0], date[1], date[2], k],
                [date[0], date[1], date[2], k + step],
            )
        }
    };
    let s_ns = zoned_step(cal, tz, origin, sd)?;
    let e_ns = zoned_step(cal, tz, origin, ed)?;
    let den = e_ns - s_ns;
    let num = dest_ns - s_ns;
    // total = r1 + (num/den)·inc·sign, as ONE correctly-rounded rational — the
    // separate multiply-then-add would round twice.
    let total = if den == 0 {
        r1 as f64
    } else {
        rational_to_f64(r1 as i128 * den + num * inc as i128 * sign as i128, den)
    };
    let expanded =
        den != 0 && round_fraction_exact(r1 / inc, sign, num.abs(), den.abs(), mode) != r1 / inc;
    let (date, nudged_ns) = if expanded { (ed, e_ns) } else { (sd, s_ns) };
    Ok((
        Nudge {
            date,
            time_ns: 0,
            nudged_ns,
            expanded,
        },
        total,
    ))
}

/// NudgeToZonedTime: round only the TIME part, against the length of the real
/// zoned day it sits in — halfway through a 25-hour day is 12:30, not 12:00 —
/// and carry a whole day when the rounded time reaches the far boundary.
fn nudge_zoned_time(
    cal: Cal,
    tz: &str,
    origin: i128,
    date: [i64; 4],
    time_ns: i128,
    sign: i64,
    inc: i128,
    unit: &str,
    mode: &str,
) -> Result<Nudge, Thrown> {
    let s_ns = zoned_step(cal, tz, origin, date)?;
    let e_ns = zoned_step(cal, tz, origin, [date[0], date[1], date[2], date[3] + sign])?;
    let step = unit_ns(unit) * inc;
    let rounded = round_increment(time_ns, step, mode);
    let beyond = rounded - (e_ns - s_ns);
    // The rounded time reached (or passed) the far day boundary: carry the day
    // and keep what is left over past it.
    if beyond.signum() as i64 != -sign {
        let r = round_increment(beyond, step, mode);
        return Ok(Nudge {
            date: [date[0], date[1], date[2], date[3] + sign],
            time_ns: r,
            nudged_ns: e_ns + r,
            expanded: true,
        });
    }
    Ok(Nudge {
        date,
        time_ns: rounded,
        nudged_ns: s_ns + rounded,
        expanded: false,
    })
}

/// BubbleRelativeDuration: a smallest unit that rounded up to a whole larger one
/// folds into it — 31 days from 1 January is 1 month, not 1 month 0 days — with
/// each larger unit's endpoint materialized as a real zoned instant.
fn bubble_relative(
    cal: Cal,
    tz: &str,
    origin: i128,
    mut date: [i64; 4],
    nudged_ns: i128,
    sign: i64,
    largest: &str,
    smallest: &str,
) -> Result<[i64; 4], Thrown> {
    const ORDER: [&str; 4] = ["year", "month", "week", "day"];
    let rank = |u: &str| ORDER.iter().position(|&x| x == u).unwrap_or(3);
    let (li, si) = (rank(largest), rank(smallest));
    let mut i = si as i64 - 1;
    while i >= li as i64 {
        let unit = ORDER[i as usize];
        // Days never bubble into weeks unless weeks are what was asked for:
        // P1M4W is not a duration Temporal produces.
        if unit != "week" || largest == "week" {
            let end: [i64; 4] = match unit {
                "year" => [date[0] + sign, 0, 0, 0],
                "month" => [date[0], date[1] + sign, 0, 0],
                _ => [date[0], date[1], date[2] + sign, 0],
            };
            if (nudged_ns - zoned_step(cal, tz, origin, end)?).signum() as i64 == -sign {
                break;
            }
            date = end;
        }
        i -= 1;
    }
    Ok(date)
}

/// RoundRelativeDuration for a ZONED anchor: a calendar smallestUnit — or "day",
/// which in a zone is just as irregular — nudges against calendar brackets;
/// a time smallestUnit rounds within the real zoned day.
fn round_relative_zoned(
    cal: Cal,
    tz: &str,
    origin: i128,
    date: [i64; 4],
    time_ns: i128,
    dest_ns: i128,
    largest: &str,
    inc: i128,
    smallest: &str,
    mode: &str,
) -> Result<([i64; 4], i128), Thrown> {
    let dsign = date
        .iter()
        .map(|x| x.signum())
        .find(|&s| s != 0)
        .unwrap_or(0);
    let sign = if (if dsign != 0 {
        dsign
    } else {
        time_ns.signum() as i64
    }) < 0
    {
        -1
    } else {
        1
    };
    let n = if matches!(smallest, "year" | "month" | "week" | "day") {
        nudge_calendar(
            cal, tz, origin, date, dest_ns, sign, inc as i64, smallest, mode,
        )?
        .0
    } else {
        nudge_zoned_time(cal, tz, origin, date, time_ns, sign, inc, smallest, mode)?
    };
    let mut out = n.date;
    if n.expanded && smallest != "week" {
        let from = if matches!(smallest, "year" | "month") {
            smallest
        } else {
            "day"
        };
        out = bubble_relative(cal, tz, origin, out, n.nudged_ns, sign, largest, from)?;
    }
    Ok((out, n.time_ns))
}

/// TemporalDurationFromInternal for a zoned result: with a DATE largestUnit the
/// time remainder balances into UNCAPPED hours (a 25-hour day differencing to
/// "day" is P1DT1H) and the days come from the date part, not from the clock.
fn zoned_duration_record(
    date: [i64; 4],
    time_ns: i128,
    largest: &str,
) -> Result<[f64; 10], Thrown> {
    if !matches!(largest, "year" | "month" | "week" | "day") {
        return balance_duration_ns(time_ns, largest);
    }
    let mut out = balance_duration_ns(time_ns, "hour")?;
    for i in 0..4 {
        out[i] = date[i] as f64;
    }
    Ok(out)
}

/// DifferenceZonedDateTimeWithRounding, as a Duration record.
pub(crate) fn diff_zoned_rounded(
    cal: Cal,
    tz: &str,
    ns1: i128,
    ns2: i128,
    largest: &str,
    inc: i128,
    smallest: &str,
    mode: &str,
) -> Result<[f64; 10], Thrown> {
    // A time largestUnit never consults the calendar or the zone: the answer is
    // the exact elapsed nanoseconds, rounded (DifferenceInstant).
    if !matches!(largest, "year" | "month" | "week" | "day") {
        return balance_duration_ns(
            round_increment(ns2 - ns1, unit_ns(smallest) * inc, mode),
            largest,
        );
    }
    let (date, time_ns) = difference_zoned(cal, tz, ns1, ns2, largest)?;
    let (date, time_ns) = if inc == 1 && smallest == "nanosecond" {
        (date, time_ns)
    } else {
        round_relative_zoned(
            cal, tz, ns1, date, time_ns, ns2, largest, inc, smallest, mode,
        )?
    };
    zoned_duration_record(date, time_ns, largest)
}

/// DifferenceZonedDateTimeWithTotal: the (fractional) total of the span ns1→ns2
/// in `unit`. For a calendar unit or "day" the fraction is measured between the
/// two zoned brackets, which is how 25 hours across a fall-back comes out as
/// exactly 1 day and 12 hours as 12/25 of one.
pub(crate) fn diff_zoned_total(
    cal: Cal,
    tz: &str,
    ns1: i128,
    ns2: i128,
    unit: &str,
) -> Result<f64, Thrown> {
    if !matches!(unit, "year" | "month" | "week" | "day") {
        return Ok(rational_to_f64(ns2 - ns1, unit_ns(unit)));
    }
    let (date, time_ns) = difference_zoned(cal, tz, ns1, ns2, unit)?;
    let dsign = date
        .iter()
        .map(|x| x.signum())
        .find(|&s| s != 0)
        .unwrap_or(0);
    let sign = if (if dsign != 0 {
        dsign
    } else {
        time_ns.signum() as i64
    }) < 0
    {
        -1
    } else {
        1
    };
    Ok(nudge_calendar(cal, tz, ns1, date, ns2, sign, 1, unit, "trunc")?.1)
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
        return Err(Thrown(
            "RangeError: Temporal result is outside the representable range".into(),
        ));
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
    let cal_end = dt_add_dur(
        cal,
        start,
        [f[0] as i64, f[1] as i64, f[2] as i64, 0, 0, 0, 0, 0, 0, 0],
    );
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
    let time_ns =
        (df[3] as i128) * DAY_NS + time_to_ns(&[df[4], df[5], df[6], df[7], df[8], df[9]]);
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
    let si = ["year", "month", "week"]
        .iter()
        .position(|&x| x == smallest)
        .unwrap_or(2);
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
    let sval = if si == 2 {
        (base[2] * 7 + base[3]) / 7
    } else {
        base[si]
    };
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
        let progress = if ud != ld {
            (ns2 - ld) as f64 / (ud - ld) as f64
        } else {
            0.0
        };
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
                // Behaviour 2 is an OPTION offset written to MINUTE precision,
                // which InterpretISODateTimeOffset then matches fuzzily against a
                // zone whose real offset has seconds; 3 is the same but written
                // with seconds, which must match exactly ("-00:45" matches
                // Africa/Monrovia's -00:44:30, "-00:45:00" does not).
                let sub_minute = off_str.matches(':').count() >= 2
                    || off_str.contains('.')
                    || off_str.contains(',')
                    || (!off_str.contains(':')
                        && off_str.bytes().filter(u8::is_ascii_digit).count() > 4);
                (
                    &t[..opos],
                    parse_offset_ns(off_str)? as i64,
                    if sub_minute { 3i8 } else { 2i8 },
                )
            } else {
                (t, tz_offset, 0i8)
            }
        }
    };
    let time = if time_str.is_empty() {
        [0i64; 6]
    } else {
        parse_iso_time(time_str)?
    };
    let f = [
        date.0, date.1, date.2, time[0], time[1], time[2], time[3], time[4], time[5],
    ];
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

/// FormatDateTimeUTCOffsetRounded: the offset a `toString()` prints, rounded to
/// the nearest minute (half-expand). A zone whose real offset has seconds —
/// Africa/Monrovia was −00:44:30 until 1972 — serializes as `-00:45` even though
/// the `offset` PROPERTY reports the full precision and the wall clock shown is
/// computed from the exact value.
fn format_offset_rounded(ns: i64) -> String {
    const MIN: i64 = 60_000_000_000;
    let (q, r) = (ns / MIN, ns % MIN);
    let minutes = if r.abs() * 2 >= MIN {
        q + if ns < 0 { -1 } else { 1 }
    } else {
        q
    };
    let sign = if minutes < 0 { '-' } else { '+' };
    let a = minutes.abs();
    format!("{sign}{:02}:{:02}", a / 60, a % 60)
}

// submodules (split out of the former monolithic temporal.rs)
pub(crate) mod calendar;
pub(crate) use calendar::*;
// The one calendar family that is not arithmetic: chinese/dangi need true new
// moons and solar terms, so they get their own astronomy.
mod astro;
mod chinese;
// The IANA time zone database: a generated table and the reader over it.
#[rustfmt::skip]
mod tzdata;
mod duration;
mod instant_zdt;
mod plain_date;
mod plain_date_time;
mod plain_time;
pub(crate) mod tzdb;
mod year_month_day;

pub(crate) fn tzdb_version() -> &'static str {
    tzdata::TZDB_VERSION
}
