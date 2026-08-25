//! The `chinese` and `dangi` calendars: the lunisolar year structure that
//! [`super::astro`]'s new moons and solar longitudes imply.
//!
//! Both calendars are the SAME algorithm evaluated on a different meridian, so
//! everything here is parameterised by one bool. The rules, in the form the
//! Temporal tests exercise:
//!
//! * A month begins on the local day containing a true new moon. Months are
//!   therefore 29 or 30 days, never anything else.
//! * A *sui* runs from the month containing one December solstice (month 11) to
//!   the month containing the next. It holds 12 or 13 lunar months.
//! * A 13-month sui inserts a leap month: the FIRST of its months that contains
//!   no *zhongqi* (no instant of solar longitude ≡ 0 mod 30°). A leap month
//!   takes the number of the month before it, so it is `M05L` when it follows
//!   `M05` — the same `M..L` code layer `hebrew` already uses.
//! * The year begins at the second month of the sui after month 11 — the third
//!   when the leap month is one of those two, which is exactly why a leap 11th
//!   or 12th month belongs to the PREVIOUS calendar year.
//! * The year NUMBER is the ISO year its first month falls in
//!   (`PlainDate.from({calendar:"chinese", year: 2025, …})` is the year that
//!   opened on 2025-01-29), which is what `relatedYear` reports.
//!
//! The year numbering means calendar year `y` is assembled from the tail of the
//! sui opened by the December solstice of ISO `y-1` and the head of the next.
//!
//! WHERE THIS DISAGREES WITH ICU4C: on 477 of the 73414 days of 1900–2100 for
//! `chinese` and 270 for `dangi`, in 13 and 9 runs. Every one is a boundary
//! decided by minutes — a new moon or a zhongqi within ~14 minutes of local
//! midnight — where ICU4C's low-precision astronomy lands on the other side.
//! The published calendars agree with THIS file, not with ICU: Chinese New Year
//! 1954-02-03, 2027-02-06 and 2030-02-03 and the leap sixth month of 1987 are
//! all as computed here, and test262's own independently-authored expectations
//! (`inLeapYear/chinese-calendar-dates.js`, `monthCode/chinese-calendar-dates.js`,
//! `daysInMonth/basic-chinese.js` and their `dangi` twins) match this file
//! exactly, including the two years where `chinese` and `dangi` themselves part
//! company (2012 and 2017). See the tests at the end of this file.

use std::cell::RefCell;
use std::collections::HashMap;

use super::super::helpers_datetime::{epoch_days_to_iso, iso_to_epoch_days};
use super::astro::{
    estimate_prior_solar_longitude, new_moon_index_before, nth_new_moon, solar_longitude,
    RD_AT_EPOCH,
};

/// December solstice, as a solar longitude.
const WINTER: f64 = 270.0;

/// The window in which these calendars are computed for the PlainMonthDay
/// reference-year search. `extreme-dates.js` states the accurate range as
/// 1900–2100 for `chinese` and 1900–2050 for `dangi`, and those bounds are what
/// reproduce the reference years the spec tabulates: searched over a wider
/// window, `dangi`'s `M08L` would find a 30-day instance in 2052, which
/// `PlainMonthDay/from/chinese-dangi-constrain-rare-leap-months.js` says must
/// not exist.
pub(crate) const REF_YEAR_LO: i64 = 1900;
pub(crate) const REF_YEAR_HI_CHINESE: i64 = 2100;
pub(crate) const REF_YEAR_HI_DANGI: i64 = 2050;

/// Offset of the calendar's reference meridian from UT, in days.
///
/// Not a modern civil time zone: it is the meridian the calendar was *computed*
/// on at the time. China used Beijing local mean time (116°25′E = 7h45m40s)
/// until the 1929 adoption of UTC+8; Korea's ran through four settings. Getting
/// this wrong moves a month whenever a new moon falls within the offset of
/// midnight, which is why R&D give it its own function.
fn zone(dangi: bool, day: i64) -> f64 {
    let y = epoch_days_to_iso(day).0;
    let hours = if dangi {
        // Seoul: 126°58′E local mean time, then 8.5 h, 9 h, 8.5 h, 9 h.
        if y < 1908 {
            3809.0 / 450.0
        } else if y < 1912 {
            8.5
        } else if y < 1954 {
            9.0
        } else if y < 1961 {
            8.5
        } else {
            9.0
        }
    } else if y < 1929 {
        1397.0 / 180.0
    } else {
        8.0
    };
    hours / 24.0
}

/// The UT moment of local midnight opening epoch day `d`, as an R.D. moment.
fn midnight(dangi: bool, d: i64) -> f64 {
    (d + RD_AT_EPOCH) as f64 - zone(dangi, d)
}

/// The local epoch day on which new moon `n` falls.
fn new_moon_day(dangi: bool, n: i64) -> i64 {
    let ut = nth_new_moon(n) - RD_AT_EPOCH as f64;
    // The zone is keyed on the local day, which is what we are computing; seed
    // it from the UT day. The two can differ only within `zone` of midnight on
    // the single day a zone rule changes, decades from any tested value.
    (ut + zone(dangi, ut.floor() as i64)).floor() as i64
}

/// Index of the new moon that opens the lunar month containing epoch day `d`.
fn month_index_of(dangi: bool, d: i64) -> i64 {
    new_moon_index_before(midnight(dangi, d + 1))
}

/// The epoch day of the last December solstice at or before day `d`: the day
/// whose local midnight-to-midnight span the sun crosses 270° in.
fn winter_solstice_on_or_before(dangi: bool, d: i64) -> i64 {
    let approx = estimate_prior_solar_longitude(WINTER, midnight(dangi, d + 1));
    let mut x = (approx - RD_AT_EPOCH as f64).floor() as i64 - 1;
    // Two or three steps in practice. The bound only matters at absurd dates,
    // where the series is meaningless anyway but must still terminate.
    for _ in 0..400 {
        if solar_longitude(midnight(dangi, x + 1)) > WINTER {
            break;
        }
        x += 1;
    }
    x
}

/// Which of the twelve zhongqi the sun has last passed, as at local midnight of
/// day `d`. Two month starts that report the same one have no zhongqi between
/// them — the leap-month test.
fn major_solar_term(dangi: bool, d: i64) -> i64 {
    let s = solar_longitude(midnight(dangi, d));
    (2 + (s / 30.0).floor() as i64).rem_euclid(12)
}

/// One solstice-to-solstice period: its months and which (if any) is leap.
#[derive(Clone, Copy)]
struct Sui {
    /// New-moon index of month 11, the month containing the opening solstice.
    first_nm: i64,
    /// Months from that month 11 to the next one: 12, or 13 in a leap sui.
    k: u8,
    /// 0, or the position (1..=12) of the leap month after month 11.
    leap: u8,
    /// Epoch day each of the `k + 1` months opens on.
    days: [i32; 15],
}

/// A calendar year: `n_months` months starting at `starts[0]`, with
/// `starts[n_months]` the following new year.
#[derive(Clone, Copy)]
pub(crate) struct CnYear {
    pub(crate) n_months: u8,
    /// 0, or the 1-based ordinal of the leap month. Its month CODE is the number
    /// one lower, so ordinal 7 is `M06L`.
    pub(crate) leap_ord: u8,
    /// New-moon index of month 1 — the global, monotone month index that makes
    /// month arithmetic across 12- and 13-month years a subtraction.
    pub(crate) first_nm: i64,
    pub(crate) starts: [i32; 15],
}

impl CnYear {
    pub(crate) fn month_start(&self, m: i64) -> i64 {
        self.starts[(m - 1).clamp(0, self.n_months as i64) as usize] as i64
    }
    pub(crate) fn days_in_month(&self, m: i64) -> i64 {
        self.month_start(m + 1) - self.month_start(m)
    }
}

thread_local! {
    static SUI_CACHE: RefCell<HashMap<(bool, i64), Sui>> = RefCell::new(HashMap::new());
    static YEAR_CACHE: RefCell<HashMap<(bool, i64), CnYear>> = RefCell::new(HashMap::new());
}

/// Recomputing a year costs ~30 evaluations of the two series, and every field
/// accessor on a date needs one, so they are memoised. The cap keeps a loop over
/// absurd year ranges from retaining unbounded memory; dropping the whole map is
/// fine because the entries are pure functions of the key.
fn cached<K: std::hash::Hash + Eq + Copy, V: Copy>(
    cache: &'static std::thread::LocalKey<RefCell<HashMap<K, V>>>,
    key: K,
    compute: impl FnOnce() -> V,
) -> V {
    if let Some(v) = cache.with(|c| c.borrow().get(&key).copied()) {
        return v;
    }
    let v = compute();
    cache.with(|c| {
        let mut m = c.borrow_mut();
        if m.len() > 4096 {
            m.clear();
        }
        m.insert(key, v);
    });
    v
}

fn sui(dangi: bool, g: i64) -> Sui {
    cached(&SUI_CACHE, (dangi, g), || {
        let s1 = winter_solstice_on_or_before(dangi, iso_to_epoch_days(g, 12, 31));
        let s2 = winter_solstice_on_or_before(dangi, iso_to_epoch_days(g + 1, 12, 31));
        let i0 = month_index_of(dangi, s1);
        // The month count comes from the new-moon INDICES, not from rounding a
        // day difference, so it is exact; the clamp only guards the case where
        // an extreme-date solstice search gave up.
        let k = (month_index_of(dangi, s2) - i0).clamp(12, 13) as u8;
        let mut days = [0i32; 15];
        for i in 0..=k as i64 {
            days[i as usize] = new_moon_day(dangi, i0 + i) as i32;
        }
        let mut leap = 0u8;
        if k == 13 {
            for i in 1..=12usize {
                if major_solar_term(dangi, days[i] as i64)
                    == major_solar_term(dangi, days[i + 1] as i64)
                {
                    leap = i as u8;
                    break;
                }
            }
            // A 13-month sui always has a zhongqi-less month; if the series has
            // degenerated (extreme dates only) put the leap month last rather
            // than leaving a 13-month year with no leap month at all.
            if leap == 0 {
                leap = 13;
            }
        }
        Sui {
            first_nm: i0,
            k,
            leap,
            days,
        }
    })
}

/// The structure of calendar year `y`.
pub(crate) fn year(dangi: bool, y: i64) -> CnYear {
    cached(&YEAR_CACHE, (dangi, y), || {
        let a = sui(dangi, y - 1);
        let b = sui(dangi, y);
        // Month 1 is two months after month 11 — three when the leap month is
        // one of those two, since a leap 11th or 12th month delays the new year.
        let ia = if a.k == 13 && a.leap <= 2 { 3usize } else { 2 };
        let ib = if b.k == 13 && b.leap <= 2 { 3usize } else { 2 };
        let mut starts = [0i32; 15];
        let mut n = 0usize;
        for i in ia..a.k as usize {
            starts[n] = a.days[i];
            n += 1;
        }
        for i in 0..=ib {
            starts[n] = b.days[i];
            n += 1;
        }
        let leap_ord = if a.k == 13 && a.leap as usize > ia {
            a.leap as usize - ia + 1
        } else if b.k == 13 && (b.leap as usize) < ib {
            a.k as usize - ia + b.leap as usize + 1
        } else {
            0
        };
        CnYear {
            n_months: (n - 1) as u8,
            leap_ord: leap_ord as u8,
            first_nm: a.first_nm + ia as i64,
            starts,
        }
    })
}

/// The calendar year containing epoch day `d`, and its structure.
pub(crate) fn year_of(dangi: bool, d: i64) -> (i64, CnYear) {
    // The new year falls in ISO Jan/Feb, so the ISO year is the answer or one
    // more than it; the loops are bounded so a degenerate extreme date cannot
    // spin.
    let mut y = epoch_days_to_iso(d).0;
    for _ in 0..3 {
        if d < year(dangi, y).starts[0] as i64 {
            y -= 1;
        } else {
            break;
        }
    }
    for _ in 0..3 {
        let cy = year(dangi, y);
        if d >= cy.starts[cy.n_months as usize] as i64 {
            y += 1;
        } else {
            return (y, cy);
        }
    }
    (y, year(dangi, y))
}

/// The (year, ordinal month, day) of an epoch day.
pub(crate) fn from_epoch_days(dangi: bool, d: i64) -> (i64, i64, i64) {
    let (y, cy) = year_of(dangi, d);
    let mut m = 1i64;
    while m < cy.n_months as i64 && d >= cy.starts[m as usize] as i64 {
        m += 1;
    }
    (y, m, d - cy.month_start(m) + 1)
}

/// Normalise a (year, ordinal month) whose month has rolled out of its year —
/// which the difference and addition probes do — using the global month index.
pub(crate) fn normalize(dangi: bool, y: i64, m: i64) -> (i64, i64, CnYear) {
    let cy = year(dangi, y);
    if (1..=cy.n_months as i64).contains(&m) {
        return (y, m, cy);
    }
    let (ny, nm) = month_from_index(dangi, cy.first_nm + m - 1);
    (ny, nm, year(dangi, ny))
}

/// Inverse of the global month index.
pub(crate) fn month_from_index(dangi: bool, idx: i64) -> (i64, i64) {
    let (y, _, _) = from_epoch_days(dangi, new_moon_day(dangi, idx));
    let cy = year(dangi, y);
    (y, idx - cy.first_nm + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leap_years(dangi: bool, lo: i64, hi: i64) -> Vec<i64> {
        (lo..hi)
            .filter(|&y| year(dangi, y).n_months == 13)
            .collect()
    }

    /// test262's own leap-year list, authored independently of this engine
    /// (`inLeapYear/chinese-calendar-dates.js`, `.../dangi-calendar-dates.js`).
    #[test]
    fn leap_years_match_test262() {
        let expected: Vec<i64> = vec![
            1971, 1974, 1976, 1979, 1982, 1984, 1987, 1990, 1993, 1995, 1998, 2001, 2004, 2006,
            2009, 2012, 2014, 2017, 2020, 2023, 2025, 2028, 2031, 2033, 2036, 2039, 2042, 2044,
            2047,
        ];
        assert_eq!(leap_years(false, 1970, 2050), expected);
        assert_eq!(leap_years(true, 1970, 2050), expected);
    }

    /// The leap month's POSITION, which is what the month code turns on — and
    /// where `chinese` and `dangi` genuinely differ (2012 and 2017), so this
    /// also pins that the two meridians are not accidentally the same.
    #[test]
    fn leap_month_ordinals_match_test262() {
        // (year, ordinal, code number) from monthCode/chinese-calendar-dates.js
        let chinese = [
            (1971, 6, 5),
            (1974, 5, 4),
            (1976, 9, 8),
            (1979, 7, 6),
            (1982, 5, 4),
            (1987, 7, 6),
            (1990, 6, 5),
            (1993, 4, 3),
            (1995, 9, 8),
            (1998, 6, 5),
            (2001, 5, 4),
            (2004, 3, 2),
            (2006, 8, 7),
            (2009, 6, 5),
            (2012, 5, 4),
            (2017, 7, 6),
            (2020, 5, 4),
            (2023, 3, 2),
            (2025, 7, 6),
            (2028, 6, 5),
            (2031, 4, 3),
            (2036, 7, 6),
            (2039, 6, 5),
            (2042, 3, 2),
            (2044, 8, 7),
            (2047, 6, 5),
        ];
        for (y, ord, num) in chinese {
            let cy = year(false, y);
            assert_eq!(cy.leap_ord as i64, ord, "chinese {y}");
            assert_eq!(cy.leap_ord as i64 - 1, num, "chinese {y} code");
        }
        // dangi differs from chinese in exactly these two years.
        for (y, ord) in [(2012, 4), (2017, 6)] {
            assert_eq!(year(true, y).leap_ord as i64, ord, "dangi {y}");
        }
    }

    /// Month lengths, against `daysInMonth/basic-chinese.js` (1971 leap, 1972
    /// common) and `with/chinese-calendar-leap-dates.js` (2001).
    #[test]
    fn month_lengths_match_test262() {
        let cases: [(i64, &[i64]); 3] = [
            (1971, &[29, 30, 29, 29, 30, 29, 30, 29, 30, 30, 30, 29, 30]),
            (1972, &[29, 30, 29, 29, 30, 29, 30, 29, 30, 30, 29, 30]),
            (2001, &[30, 30, 29, 30, 29, 30, 29, 29, 30, 29, 30, 29, 30]),
        ];
        for (y, lens) in cases {
            let cy = year(false, y);
            assert_eq!(cy.n_months as usize, lens.len(), "{y} month count");
            for (i, &len) in lens.iter().enumerate() {
                assert_eq!(cy.days_in_month(i as i64 + 1), len, "{y} month {}", i + 1);
                assert!(len == 29 || len == 30);
            }
        }
    }

    /// Chinese New Year, against dates published far outside this engine. Three
    /// of these (1954, 2027, 2030) are years where ICU4C puts the new year one
    /// day earlier — the new moon lands within 8 minutes of local midnight.
    #[test]
    fn new_year_matches_published_dates() {
        for (y, iso) in [
            (1900, (1900, 1, 31)),
            (1954, (1954, 2, 3)),
            (1971, (1971, 1, 27)),
            (1999, (1999, 2, 16)),
            (2000, (2000, 2, 5)),
            (2012, (2012, 1, 23)),
            (2020, (2020, 1, 25)),
            (2025, (2025, 1, 29)),
            (2027, (2027, 2, 6)),
            (2030, (2030, 2, 3)),
        ] {
            let start = year(false, y).starts[0] as i64;
            assert_eq!(epoch_days_to_iso(start), iso, "chinese new year {y}");
        }
    }

    /// Round-tripping every day of 1900–2100 is what `until`/`since`/`with` all
    /// lean on, and it is also the property that would break first if the
    /// year-assembly logic mis-joined two sui. The 29-or-30 assertion is what
    /// `cal_month_day_constrain` relies on when it clamps a PlainMonthDay day to
    /// 30 before looking for a reference year.
    #[test]
    fn every_day_round_trips() {
        for &dangi in &[false, true] {
            let lo = iso_to_epoch_days(1900, 1, 1);
            let hi = iso_to_epoch_days(2100, 12, 31);
            let mut d = lo;
            let mut prev_index = None;
            while d <= hi {
                let (y, m, dd) = from_epoch_days(dangi, d);
                let cy = year(dangi, y);
                assert!((1..=cy.n_months as i64).contains(&m));
                let len = cy.days_in_month(m);
                assert!(len == 29 || len == 30, "{y}-{m} is {len} days");
                assert_eq!(cy.month_start(m) + dd - 1, d);
                let idx = cy.first_nm + m - 1;
                if let Some(p) = prev_index {
                    assert!(idx == p || idx == p + 1, "month index jumped at {d}");
                }
                prev_index = Some(idx);
                assert_eq!(month_from_index(dangi, idx), (y, m));
                d += 1;
            }
        }
    }

    /// The absurd dates `PlainDate/from/extreme-dates.js` constructs must
    /// terminate and stay self-consistent. Only that: 250 millennia out the
    /// series has no meaning at all — its own polynomial arguments have
    /// diverged — and the test file says so, asking only that construction not
    /// throw. So this checks the structural invariants the rest of the engine
    /// leans on (a well-formed 12- or 13-month year of positive months that
    /// round-trips), NOT that months are 29 or 30 days, which the monotonicity
    /// clamp in `nth_new_moon` cannot promise once the series has saturated it.
    #[test]
    fn extreme_years_terminate() {
        for &dangi in &[false, true] {
            for y in [-250_000, -3000, 0, 1000, 250_000] {
                let cy = year(dangi, y);
                assert!(cy.n_months == 12 || cy.n_months == 13, "{y}");
                let start = cy.starts[0] as i64;
                for m in 1..=cy.n_months as i64 {
                    assert!(cy.days_in_month(m) > 0, "{y}-{m} is not a positive month");
                }
                for d in start..cy.starts[cy.n_months as usize] as i64 {
                    let (ry, rm, rd) = from_epoch_days(dangi, d);
                    assert_eq!(ry, y, "day {d} left year {y}");
                    assert_eq!(cy.month_start(rm) + rd - 1, d);
                }
            }
        }
    }
}
