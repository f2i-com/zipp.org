//! The reader over the generated IANA time zone table (`tzdata.rs`).
//!
//! Everything Temporal and ECMA-402 need from the tz database goes through
//! here: identifier lookup (`GetAvailableNamedTimeZoneIdentifier`), the offset
//! at an instant (`GetNamedTimeZoneOffsetNanoseconds`), the candidate instants
//! for a wall clock (`GetNamedTimeZoneEpochNanoseconds` — 0 of them inside a
//! spring-forward gap, 2 inside a fall-back repeat), the transition search
//! behind `ZonedDateTime.prototype.getTimeZoneTransition`, and the primary
//! identifier list behind `Intl.supportedValuesOf("timeZone")`.
//!
//! A zone is stored as an explicit ascending transition list plus the annual
//! `Rule … max …` lines that govern every year from `fin_year` on — the same
//! split a TZif file makes between its transition array and its proleptic TZ
//! string. Times are seconds here; the callers scale to nanoseconds.

use super::tzdata::{FinalRule, FINALS, IDS, TRANS_AT, TRANS_OFF, ZONES};

/// A resolved time zone identifier: the canonical spelling of what the caller
/// wrote (`"africa/cairo"` → `"Africa/Cairo"`) and the zone it denotes.
#[derive(Clone, Copy)]
pub(crate) struct TzId {
    pub(crate) canonical: &'static str,
    pub(crate) zone: u16,
}

/// GetAvailableNamedTimeZoneIdentifier: match a Zone or Link name
/// ASCII-case-insensitively. `None` means the identifier is not in the
/// database, which every caller turns into a RangeError — the whole point of
/// shipping the table is that an unknown name fails loudly instead of
/// silently formatting as UTC.
pub(crate) fn lookup(id: &str) -> Option<TzId> {
    let i = IDS
        .binary_search_by(|(name, _)| cmp_ascii_ci(name, id))
        .ok()?;
    let (canonical, zone) = IDS[i];
    Some(TzId { canonical, zone })
}

/// `str::cmp` over ASCII-lowercased bytes — the order `IDS` is generated in.
fn cmp_ascii_ci(a: &str, b: &str) -> core::cmp::Ordering {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    let n = x.len().min(y.len());
    for i in 0..n {
        let o = x[i].to_ascii_lowercase().cmp(&y[i].to_ascii_lowercase());
        if o != core::cmp::Ordering::Equal {
            return o;
        }
    }
    x.len().cmp(&y.len())
}

/// The [[PrimaryIdentifier]] of a zone — what `TimeZoneEquals` compares, so
/// that `Asia/Calcutta` and `Asia/Kolkata` are the same zone.
pub(crate) fn primary(zone: u16) -> &'static str {
    ZONES[zone as usize].name
}

/// AvailableCanonicalTimeZones: the primary identifiers, sorted and unique.
/// `Etc/UTC` and `Etc/GMT` both carry the primary identifier "UTC", so the
/// dedupe below is what keeps them out of the list (test262's
/// `isCanonicalizedStructurallyValidTimeZoneName` rejects both spellings).
pub(crate) fn primary_ids() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = ZONES.iter().map(|z| z.name).collect();
    v.sort_unstable();
    v.dedup();
    v
}

// ── civil date arithmetic (proleptic Gregorian, days since 1970-01-01) ──

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn days_in_month(y: i64, m: u8) -> i64 {
    const MD: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if m == 2 && is_leap(y) {
        29
    } else {
        MD[m as usize - 1]
    }
}

fn epoch_day(y: i64, m: u8, d: i64) -> i64 {
    let yy = y - if m <= 2 { 1 } else { 0 };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = ((m as i64) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The civil year an epoch-second falls in (used only to bracket which years'
/// annual rules can matter, so being off by the zone's offset is harmless —
/// the caller always scans a year either side).
fn civil_year(sec: i64) -> i64 {
    let z = sec.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    y + if mp >= 10 { 1 } else { 0 }
}

/// The epoch day a `Rule` ON column selects in a given year and month.
fn on_day(y: i64, r: &FinalRule) -> i64 {
    match r.on {
        // A plain day-of-month.
        0 => epoch_day(y, r.month, r.dom as i64),
        // `lastSun`: the last such weekday in the month.
        1 => {
            let b = epoch_day(y, r.month, days_in_month(y, r.month));
            b - (b + 4 - r.dow as i64).rem_euclid(7)
        }
        // `Sun>=8`: the first such weekday on or after `dom`.
        2 => {
            let b = epoch_day(y, r.month, r.dom as i64);
            b + (r.dow as i64 - (b + 4)).rem_euclid(7)
        }
        // `Sat<=30`: the last such weekday on or before `dom`.
        _ => {
            let b = epoch_day(y, r.month, r.dom as i64);
            b - (b + 4 - r.dow as i64).rem_euclid(7)
        }
    }
}

/// The transitions the annual rules produce in `year`, in order, as
/// (instant, UTC offset after it). At most `nfin` of them (2 in practice).
fn final_transitions(zone: u16, year: i64, out: &mut Vec<(i64, i32)>) {
    let z = &ZONES[zone as usize];
    out.clear();
    if z.nfin == 0 || year < z.fin_year as i64 {
        return;
    }
    let rules = &FINALS[z.fin as usize..z.fin as usize + z.nfin as usize];
    // The `save` in force entering `year`. In the first governed year that is
    // whatever the last explicit transition left; afterwards it is the save of
    // the last rule of the preceding year (which is why a southern-hemisphere
    // zone correctly starts January already on DST).
    let mut save = if year == z.fin_year as i64 {
        let last = if z.ntr > 0 {
            TRANS_OFF[(z.tr + z.ntr - 1) as usize]
        } else {
            z.init
        };
        last - z.std
    } else {
        rules[rules.len() - 1].save
    };
    for r in rules {
        let local = on_day(year, r) * 86400 + r.at as i64;
        // The AT column is wall time unless suffixed: `s` is standard time (so
        // the current save must not be added) and `u` is UT (neither applies).
        let off = match r.atq {
            2 => 0,
            1 => z.std,
            _ => z.std + save,
        };
        save = r.save;
        out.push((local - off as i64, z.std + save));
    }
}

/// GetNamedTimeZoneOffsetNanoseconds, in seconds: the UTC offset of `zone` at
/// the instant `t`.
pub(crate) fn offset_seconds(zone: u16, t: i64) -> i32 {
    let z = &ZONES[zone as usize];
    let (lo, hi) = (z.tr as usize, (z.tr + z.ntr) as usize);
    let at = &TRANS_AT[lo..hi];
    let i = at.partition_point(|&x| x <= t);
    let mut off = if i > 0 { TRANS_OFF[lo + i - 1] } else { z.init };
    // Past the explicit list the annual rules take over. Scanning the year
    // either side covers a transition that lands in the neighbouring civil
    // year once the offset is applied.
    if i == at.len() && z.nfin > 0 {
        let mut best = if i > 0 { at[i - 1] } else { i64::MIN };
        let y = civil_year(t);
        let mut buf = Vec::with_capacity(4);
        for yy in (y - 1)..=(y + 1) {
            final_transitions(zone, yy, &mut buf);
            for &(tt, oo) in buf.iter() {
                if tt <= t && tt >= best {
                    best = tt;
                    off = oo;
                }
            }
        }
    }
    off
}

/// GetNamedTimeZoneEpochNanoseconds, in seconds: the instants whose local wall
/// clock in `zone` is `local`. Zero of them inside a spring-forward gap, two
/// inside a fall-back repeat, one otherwise — returned ascending.
///
/// The ±1-day probe is the algorithm the specification itself sketches: no UTC
/// offset reaches 24 hours, so the offsets in force a day either side of the
/// wall clock are the only two candidates worth testing.
pub(crate) fn possible_instants(zone: u16, local: i64) -> Vec<i64> {
    let before = offset_seconds(zone, local - 86400) as i64;
    let after = offset_seconds(zone, local + 86400) as i64;
    let mut out: Vec<i64> = Vec::with_capacity(2);
    for off in [before, after] {
        let t = local - off;
        if offset_seconds(zone, t) as i64 == off && !out.contains(&t) {
            out.push(t);
        }
    }
    out.sort_unstable();
    out
}

/// The first transition strictly after `t`, or `None` when the zone has no
/// later one (a zone with no annual rule eventually runs out).
pub(crate) fn next_transition(zone: u16, t: i64) -> Option<i64> {
    let z = &ZONES[zone as usize];
    let (lo, hi) = (z.tr as usize, (z.tr + z.ntr) as usize);
    let at = &TRANS_AT[lo..hi];
    let i = at.partition_point(|&x| x <= t);
    if i < at.len() {
        return Some(at[i]);
    }
    if z.nfin == 0 {
        return None;
    }
    // The annual rules repeat, so a transition after `t` is in this civil year
    // or the next one; scan three to be safe against the offset shifting the
    // year boundary.
    //
    // The rules need not start the year after the explicit list ends: Egypt
    // dropped DST in 2015 and reintroduced it with a `max` rule in 2023, so
    // Africa/Cairo's last explicit transition is 2014-09-25 and `fin_year` is
    // 2023. Scanning only around `t` found nothing in that eight-year gap and
    // reported that the zone never changes offset again, which made
    // `getTimeZoneTransition("next")` null from 2014 on. Start no earlier than
    // the year before the rules take over.
    let y = civil_year(t).max(z.fin_year as i64 - 1);
    let mut buf = Vec::with_capacity(4);
    for yy in (y - 1)..=(y + 2) {
        final_transitions(zone, yy, &mut buf);
        for &(tt, _) in buf.iter() {
            if tt > t && offset_seconds(zone, tt - 1) != offset_seconds(zone, tt) {
                return Some(tt);
            }
        }
    }
    None
}

/// The last transition strictly before `t`, or `None` when the zone has none.
pub(crate) fn previous_transition(zone: u16, t: i64) -> Option<i64> {
    let z = &ZONES[zone as usize];
    let (lo, hi) = (z.tr as usize, (z.tr + z.ntr) as usize);
    let at = &TRANS_AT[lo..hi];
    if z.nfin > 0 {
        let y = civil_year(t);
        let mut buf = Vec::with_capacity(4);
        let mut best: Option<i64> = None;
        for yy in ((y - 2)..=(y + 1)).rev() {
            final_transitions(zone, yy, &mut buf);
            for &(tt, _) in buf.iter() {
                if tt < t
                    && best.map_or(true, |b| tt > b)
                    && at.last().map_or(true, |&l| tt > l)
                    && offset_seconds(zone, tt - 1) != offset_seconds(zone, tt)
                {
                    best = Some(tt);
                }
            }
            if best.is_some() {
                break;
            }
        }
        if best.is_some() {
            return best;
        }
    }
    let i = at.partition_point(|&x| x < t);
    if i > 0 {
        Some(at[i - 1])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_lookup_is_case_insensitive_and_canonicalizes() {
        assert_eq!(lookup("africa/cairo").unwrap().canonical, "Africa/Cairo");
        assert_eq!(lookup("eTc/gMt+1").unwrap().canonical, "Etc/GMT+1");
        // Links keep their own spelling but resolve to their target's zone.
        assert_eq!(lookup("Asia/Calcutta").unwrap().canonical, "Asia/Calcutta");
        assert_eq!(
            primary(lookup("Asia/Calcutta").unwrap().zone),
            primary(lookup("Asia/Kolkata").unwrap().zone)
        );
        // ECMA-402 folds the tzdb's two zero-offset Zones onto "UTC".
        assert_eq!(primary(lookup("Etc/GMT").unwrap().zone), "UTC");
        assert_eq!(primary(lookup("GMT").unwrap().zone), "UTC");
        assert!(lookup("Etc/GMT+13").is_none());
        assert!(lookup("Bogus/Zone").is_none());
    }

    #[test]
    fn offsets_span_transitions_and_annual_rules() {
        let ny = lookup("America/New_York").unwrap().zone;
        // 2021-03-14T06:59:59Z is still EST; one second later is EDT.
        assert_eq!(offset_seconds(ny, 1_615_705_199), -18000);
        assert_eq!(offset_seconds(ny, 1_615_705_200), -14400);
        // A year past the annual-rule cutover, and one far in the future.
        assert_eq!(offset_seconds(ny, 1_700_000_000), -18000);
        assert_eq!(offset_seconds(ny, 4_102_444_800), -18000);
        // Southern hemisphere: January is DST.
        let syd = lookup("Australia/Sydney").unwrap().zone;
        assert_eq!(offset_seconds(syd, 1_704_067_200), 39600);
        // Half-hour and three-quarter-hour zones.
        assert_eq!(
            offset_seconds(lookup("Asia/Kolkata").unwrap().zone, 0),
            19800
        );
        assert_eq!(
            offset_seconds(lookup("Pacific/Chatham").unwrap().zone, 0),
            45900
        );
    }

    #[test]
    fn gaps_have_no_instant_and_repeats_have_two() {
        let ny = lookup("America/New_York").unwrap().zone;
        // 2021-03-14 02:30 local never happened.
        let gap = epoch_day(2021, 3, 14) * 86400 + 2 * 3600 + 1800;
        assert!(possible_instants(ny, gap).is_empty());
        // 2021-11-07 01:30 local happened twice.
        let fold = epoch_day(2021, 11, 7) * 86400 + 3600 + 1800;
        assert_eq!(possible_instants(ny, fold).len(), 2);
        // An ordinary wall clock has exactly one instant.
        let plain = epoch_day(2021, 6, 1) * 86400 + 12 * 3600;
        assert_eq!(possible_instants(ny, plain).len(), 1);
    }

    #[test]
    fn transition_search_crosses_the_annual_rule_boundary() {
        let ny = lookup("America/New_York").unwrap().zone;
        assert_eq!(next_transition(ny, 1_615_705_199), Some(1_615_705_200));
        // The one before the 2021 spring forward is the 2020 fall back,
        // 2020-11-01T06:00Z.
        assert_eq!(previous_transition(ny, 1_615_705_200), Some(1_604_210_400));
        // Far past the explicit list both directions come from the annual rule:
        // 2100-01-01T00:00Z sits between the 2099 fall back and the 2100 spring
        // forward, and each really changes the offset.
        let t = 4_102_444_800;
        let n = next_transition(ny, t).unwrap();
        let p = previous_transition(ny, t).unwrap();
        assert!(p < t && t < n);
        assert_eq!(
            (offset_seconds(ny, n - 1), offset_seconds(ny, n)),
            (-18000, -14400)
        );
        assert_eq!(
            (offset_seconds(ny, p - 1), offset_seconds(ny, p)),
            (-14400, -18000)
        );
        // A zone that has never had a transition.
        let utc = lookup("UTC").unwrap().zone;
        assert_eq!(next_transition(utc, 0), None);
        assert_eq!(previous_transition(utc, 0), None);
    }
}
