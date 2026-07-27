//! Non-ISO calendars for Temporal.
//!
//! Every Temporal instance keeps its *ISO* date in `HeapObj::Temporal.fields`
//! and its calendar as a `u8` in the `Vm::temporal_cal` side table (absent =
//! `iso8601`). That mirrors the spec, where the internal slots are
//! `[[ISODate]]` + `[[Calendar]]` and the calendar is a pure *view*:
//! `CalendarISOToDate` projects the ISO date into calendar fields
//! (era/eraYear/year/month/monthCode/day) and `CalendarDateToISO` maps a field
//! bag back. Keeping ISO as the storage form means range checks, the epoch-day
//! arithmetic and the serializers stay exactly as they were for `iso8601`.
//!
//! Only the *arithmetic* calendars live here — those whose year/month lengths
//! are a closed-form function of the year, needing no astronomical or CLDR
//! data. The lunisolar ones (chinese/dangi/hebrew) and the observational
//! islamic variants (islamic-umalqura and friends) are deliberately absent:
//! they need month-length tables or new-moon computation we do not have, and a
//! fake answer would be worse than the honest `RangeError`.
//!
//! Formulas are the standard fixed-date (R.D.) ones from Reingold &
//! Dershowitz, *Calendrical Calculations*; `RD_AT_EPOCH` converts between R.D.
//! (day 1 = 0001-01-01 proleptic Gregorian) and this engine's epoch days
//! (day 0 = 1970-01-01).

use super::super::helpers_datetime::{epoch_days_to_iso, iso_to_epoch_days};

/// R.D. of 1970-01-01, the origin of this engine's `epoch_days`.
const RD_AT_EPOCH: i64 = 719_163;

/// The calendars this engine implements, as stored in `Vm::temporal_cal`.
/// The discriminants are persisted in that table, so they must stay stable.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Cal {
    Iso = 0,
    Gregory = 1,
    Buddhist = 2,
    Roc = 3,
    Japanese = 4,
    Coptic = 5,
    Ethiopic = 6,
    Ethioaa = 7,
    IslamicCivil = 8,
    IslamicTbla = 9,
}

impl Cal {
    pub(crate) fn from_u8(n: u8) -> Cal {
        match n {
            1 => Cal::Gregory,
            2 => Cal::Buddhist,
            3 => Cal::Roc,
            4 => Cal::Japanese,
            5 => Cal::Coptic,
            6 => Cal::Ethiopic,
            7 => Cal::Ethioaa,
            8 => Cal::IslamicCivil,
            9 => Cal::IslamicTbla,
            _ => Cal::Iso,
        }
    }

    /// The canonical Unicode calendar identifier — what `calendarId` reports and
    /// what a `[u-ca=…]` annotation serializes.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Cal::Iso => "iso8601",
            Cal::Gregory => "gregory",
            Cal::Buddhist => "buddhist",
            Cal::Roc => "roc",
            Cal::Japanese => "japanese",
            Cal::Coptic => "coptic",
            Cal::Ethiopic => "ethiopic",
            Cal::Ethioaa => "ethioaa",
            Cal::IslamicCivil => "islamic-civil",
            Cal::IslamicTbla => "islamic-tbla",
        }
    }

    /// Whether the calendar reports `era`/`eraYear` (and therefore accepts them
    /// as input fields). `iso8601` does not; every calendar here otherwise does.
    pub(crate) fn has_eras(self) -> bool {
        self != Cal::Iso
    }

    /// Gregorian-structured calendars share the ISO month/day layout and differ
    /// only in the year numbering, so their date math IS the ISO date math.
    fn gregorian_like(self) -> Option<i64> {
        // Offset added to the ISO year to get the calendar year.
        match self {
            Cal::Gregory | Cal::Japanese => Some(0),
            Cal::Buddhist => Some(543),
            Cal::Roc => Some(-1911),
            _ => None,
        }
    }

    /// Coptic-structured calendars: 12 months of 30 days plus a short 13th, a
    /// 4-year leap cycle, differing only by the year offset from the Coptic era.
    fn coptic_like(self) -> Option<i64> {
        // Coptic year = calendar year + offset. The Coptic era (Diocletian, 284
        // CE) runs 276 years behind the Ethiopic one (Amete Mihret, 8 CE), and
        // Amete Alem is a further 5500 years ahead of Amete Mihret.
        match self {
            Cal::Coptic => Some(0),
            Cal::Ethiopic => Some(-276),
            Cal::Ethioaa => Some(-5776),
            _ => None,
        }
    }

    /// Tabular (arithmetic) Islamic calendars: the 30-year cycle, differing only
    /// in whether the epoch is the civil (Friday) or astronomical (Thursday) one.
    fn islamic_epoch(self) -> Option<i64> {
        match self {
            // R.D. 227015 / 227014, expressed in epoch days.
            Cal::IslamicCivil => Some(227_015 - RD_AT_EPOCH),
            Cal::IslamicTbla => Some(227_014 - RD_AT_EPOCH),
            _ => None,
        }
    }
}

/// Resolve a calendar identifier (ASCII case-insensitive) to its calendar,
/// applying the CLDR aliases that `CanonicalizeCalendar` must accept
/// (`islamicc` → `islamic-civil`, `ethiopic-amete-alem` → `ethioaa`).
/// Unsupported-but-real ids (`chinese`, `hebrew`, `islamic-umalqura`, …) return
/// `None` and the caller reports them as unsupported, not as malformed.
pub(crate) fn calendar_by_id(s: &str) -> Option<Cal> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    Some(match lower.as_str() {
        "iso8601" => Cal::Iso,
        "gregory" | "gregorian" => Cal::Gregory,
        "buddhist" => Cal::Buddhist,
        "roc" | "minguo" => Cal::Roc,
        "japanese" => Cal::Japanese,
        "coptic" => Cal::Coptic,
        "ethiopic" => Cal::Ethiopic,
        "ethioaa" | "ethiopic-amete-alem" => Cal::Ethioaa,
        "islamic-civil" | "islamicc" => Cal::IslamicCivil,
        "islamic-tbla" => Cal::IslamicTbla,
        _ => return None,
    })
}

// ── Coptic-structured helpers ───────────────────────────────────────────────

/// Epoch day of Coptic `y`-01-01 (R.D. 103605 is Coptic 1-01-01).
fn coptic_year_start(y: i64) -> i64 {
    103_605 - 1 + 365 * (y - 1) + y.div_euclid(4) + 1 - RD_AT_EPOCH
}

fn coptic_leap(y: i64) -> bool {
    y.rem_euclid(4) == 3
}

// ── Tabular-Islamic helpers ─────────────────────────────────────────────────

/// Whether tabular-Islamic year `y` is a leap year (the 11-in-30 cycle).
fn islamic_leap(y: i64) -> bool {
    (14 + 11 * y).rem_euclid(30) < 11
}

/// Epoch day of tabular-Islamic `y`-01-01 for a given epoch. 354 days a year
/// plus the 11 intercalary days the 30-year cycle has inserted so far.
fn islamic_year_start(epoch: i64, y: i64) -> i64 {
    epoch + 354 * (y - 1) + (3 + 11 * y).div_euclid(30)
}

// ── The calendar interface ──────────────────────────────────────────────────

/// `CalendarDateMonthsInYear`: 13 for the Coptic-structured calendars, 12 for
/// everything else here (no lunisolar calendar is implemented).
pub(crate) fn cal_months_in_year(c: Cal, _y: i64) -> i64 {
    if c.coptic_like().is_some() {
        13
    } else {
        12
    }
}

/// `CalendarDateDaysInMonth` for a calendar year/ordinal month.
pub(crate) fn cal_days_in_month(c: Cal, y: i64, m: i64) -> i64 {
    if let Some(off) = c.gregorian_like() {
        return super::super::helpers_datetime::days_in_month(y - off, m);
    }
    if let Some(off) = c.coptic_like() {
        // Twelve 30-day months then a 5-day (6 in a leap year) epagomenal month.
        return if m == 13 {
            if coptic_leap(y + off) {
                6
            } else {
                5
            }
        } else {
            30
        };
    }
    if c.islamic_epoch().is_some() {
        // Alternating 30/29, with a 30-day twelfth month in leap years.
        return if m % 2 == 1 || (m == 12 && islamic_leap(y)) { 30 } else { 29 };
    }
    super::super::helpers_datetime::days_in_month(y, m)
}

/// The greatest length calendar month `m` ever has, searching back over a full
/// leap cycle from `y` — what `overflow: "constrain"` clamps a PlainMonthDay day
/// to, since its reference year is chosen afterwards.
pub(crate) fn cal_month_max_days(c: Cal, y: i64, m: i64) -> i64 {
    // 40 years covers both the 4-year Coptic/Gregorian cycle and the 30-year
    // tabular-Islamic one.
    (0..40).map(|k| cal_days_in_month(c, y - k, m)).max().unwrap_or(0)
}

/// `CalendarDateInLeapYear`.
pub(crate) fn cal_in_leap_year(c: Cal, y: i64) -> bool {
    if let Some(off) = c.gregorian_like() {
        return super::super::helpers_datetime::is_leap_year(y - off);
    }
    if let Some(off) = c.coptic_like() {
        return coptic_leap(y + off);
    }
    if c.islamic_epoch().is_some() {
        return islamic_leap(y);
    }
    super::super::helpers_datetime::is_leap_year(y)
}

/// `CalendarDateDaysInYear`.
pub(crate) fn cal_days_in_year(c: Cal, y: i64) -> i64 {
    if c.gregorian_like().is_some() || c == Cal::Iso {
        return if cal_in_leap_year(c, y) { 366 } else { 365 };
    }
    if c.coptic_like().is_some() {
        return if cal_in_leap_year(c, y) { 366 } else { 365 };
    }
    if c.islamic_epoch().is_some() {
        return if islamic_leap(y) { 355 } else { 354 };
    }
    365
}

/// Epoch day of calendar `y`-`m`-`d`. Callers regulate `m`/`d` first; a month
/// outside 1..monthsInYear still produces a well-defined (rolled) day so the
/// probing loops in the difference algorithms cannot trap.
pub(crate) fn cal_to_epoch_days(c: Cal, y: i64, m: i64, d: i64) -> i64 {
    if let Some(off) = c.gregorian_like() {
        return iso_to_epoch_days(y - off, m, d);
    }
    if let Some(off) = c.coptic_like() {
        return coptic_year_start(y + off) + 30 * (m - 1) + d - 1;
    }
    if let Some(epoch) = c.islamic_epoch() {
        // 29*(m-1) + floor(m/2) days precede month m (the 30/29 alternation).
        return islamic_year_start(epoch, y) + 29 * (m - 1) + m.div_euclid(2) + d - 1;
    }
    iso_to_epoch_days(y, m, d)
}

/// Epoch day of calendar year `y`, month 1, day 1 — the anchor the ordinal
/// month/day walk starts from.
fn cal_year_start(c: Cal, y: i64) -> i64 {
    cal_to_epoch_days(c, y, 1, 1)
}

/// `CalendarISOToDate`'s year/month/day: project an epoch day into the calendar.
pub(crate) fn cal_from_epoch_days(c: Cal, ed: i64) -> (i64, i64, i64) {
    if let Some(off) = c.gregorian_like() {
        let (y, m, d) = epoch_days_to_iso(ed);
        return (y + off, m, d);
    }
    // Closed-form year estimate, then a correction step (the estimates are exact
    // for in-range dates; the loop is the guard that keeps a boundary day from
    // landing in the wrong year).
    let mut y = if let Some(off) = c.coptic_like() {
        (4 * (ed + RD_AT_EPOCH - 103_605) + 1463).div_euclid(1461) - off
    } else if let Some(epoch) = c.islamic_epoch() {
        (30 * (ed - epoch) + 10646).div_euclid(10631)
    } else {
        let (iy, m, d) = epoch_days_to_iso(ed);
        return (iy, m, d);
    };
    while cal_year_start(c, y) > ed {
        y -= 1;
    }
    while cal_year_start(c, y + 1) <= ed {
        y += 1;
    }
    let mut rem = ed - cal_year_start(c, y);
    let mut m = 1;
    while m < cal_months_in_year(c, y) && rem >= cal_days_in_month(c, y, m) {
        rem -= cal_days_in_month(c, y, m);
        m += 1;
    }
    (y, m, rem + 1)
}

/// `CalendarISOToDate` for an ISO date.
pub(crate) fn cal_from_iso(c: Cal, y: i64, m: i64, d: i64) -> (i64, i64, i64) {
    if c == Cal::Iso {
        return (y, m, d);
    }
    if let Some(off) = c.gregorian_like() {
        return (y + off, m, d);
    }
    cal_from_epoch_days(c, iso_to_epoch_days(y, m, d))
}

/// `CalendarISOToDate`'s `[[Era]]`/`[[EraYear]]`, or `None` for `iso8601`.
/// The Japanese eras need the whole date because an era boundary falls mid-year.
pub(crate) fn cal_era(c: Cal, y: i64, m: i64, d: i64) -> Option<(&'static str, i64)> {
    Some(match c {
        Cal::Iso => return None,
        Cal::Gregory => {
            if y >= 1 {
                ("ce", y)
            } else {
                ("bce", 1 - y)
            }
        }
        Cal::Buddhist => ("be", y),
        Cal::Roc => {
            if y >= 1 {
                ("roc", y)
            } else {
                ("broc", 1 - y)
            }
        }
        Cal::Japanese => return Some(japanese_era(y, m, d)),
        Cal::Coptic => ("am", y),
        Cal::Ethiopic => {
            // Amete Mihret from year 1; earlier dates fall back to Amete Alem,
            // which is offset 5500 years before it.
            if y >= 1 {
                ("am", y)
            } else {
                ("aa", y + 5500)
            }
        }
        Cal::Ethioaa => ("aa", y),
        Cal::IslamicCivil | Cal::IslamicTbla => {
            if y >= 1 {
                ("ah", y)
            } else {
                ("bh", 1 - y)
            }
        }
    })
}

/// The Japanese era of a Gregorian-numbered date. Temporal supports only the
/// five modern regnal eras; anything earlier reports the Gregorian `ce`/`bce`.
///
/// THE MEIJI CUTOFF IS 1873-01-01, not the regnal 1868-10-23: Japan adopted the
/// Gregorian calendar at Meiji 6, and dates before that are lunisolar, so
/// Temporal declines to label them. test262 pins this
/// (`PlainDate/from/era-boundary-japanese.js`: "Meiji 1 still resolves to CE
/// 1868 after era start date"; `japanese-pre-meiji.js` states the rule). ICU
/// instead carries ~230 historical eras and starts meiji at 1868-10-23, so
/// `Intl.DateTimeFormat` and this disagree on pre-1873 era LABELS — the
/// year/month/day always agree.
///
/// Each era is anchored at a fixed year (eraYear = year − anchor), which is
/// what makes CalendarResolveFields' lenient remapping of an out-of-bounds era
/// year fall out for free: "reiwa 0" is simply 2018, relabelled on read.
fn japanese_era(y: i64, m: i64, d: i64) -> (&'static str, i64) {
    let after = |sy: i64, sm: i64, sd: i64| (y, m, d) >= (sy, sm, sd);
    if after(2019, 5, 1) {
        ("reiwa", y - 2018)
    } else if after(1989, 1, 8) {
        ("heisei", y - 1988)
    } else if after(1926, 12, 25) {
        ("showa", y - 1925)
    } else if after(1912, 7, 30) {
        ("taisho", y - 1911)
    } else if after(1873, 1, 1) {
        ("meiji", y - 1867)
    } else if y >= 1 {
        ("ce", y)
    } else {
        ("bce", 1 - y)
    }
}

/// `CalendarResolveFields`' era → year step: map an (era, eraYear) pair to the
/// calendar's arithmetic year, canonicalizing era aliases. `None` means the era
/// code is not one of this calendar's (a RangeError for the caller).
///
/// Out-of-bounds era years are resolved *leniently* rather than rejected (the
/// spec's recommendation for regnal eras): `reiwa 0` simply means the year
/// before Reiwa 1, and the resulting date reports whatever era it actually
/// falls in. That falls straight out of anchoring each era at a fixed year.
pub(crate) fn cal_resolve_era(c: Cal, era: &str, era_year: i64) -> Option<i64> {
    let anchored = |anchor: i64| Some(anchor + era_year);
    match c {
        Cal::Iso => None,
        Cal::Gregory => match era {
            "ce" | "ad" => Some(era_year),
            "bce" | "bc" => Some(1 - era_year),
            _ => None,
        },
        Cal::Buddhist => (era == "be").then_some(era_year),
        Cal::Roc => match era {
            "roc" => Some(era_year),
            "broc" => Some(1 - era_year),
            _ => None,
        },
        Cal::Japanese => match era {
            "ce" | "ad" => Some(era_year),
            "bce" | "bc" => Some(1 - era_year),
            "meiji" => anchored(1867),
            "taisho" => anchored(1911),
            "showa" => anchored(1925),
            "heisei" => anchored(1988),
            "reiwa" => anchored(2018),
            _ => None,
        },
        Cal::Coptic => (era == "am").then_some(era_year),
        Cal::Ethiopic => match era {
            "am" => Some(era_year),
            "aa" => Some(era_year - 5500),
            _ => None,
        },
        Cal::Ethioaa => (era == "aa").then_some(era_year),
        Cal::IslamicCivil | Cal::IslamicTbla => match era {
            "ah" => Some(era_year),
            "bh" => Some(1 - era_year),
            _ => None,
        },
    }
}

/// `CalendarDateToISO`: a regulated calendar date → the ISO date it denotes,
/// or `None` when `overflow: reject` and a field is out of range. `constrain`
/// clamps the month to monthsInYear and the day to daysInMonth.
pub(crate) fn cal_date_to_iso(
    c: Cal,
    y: i64,
    m: i64,
    d: i64,
    reject: bool,
) -> Option<(i64, i64, i64)> {
    let miy = cal_months_in_year(c, y);
    if reject && !(1..=miy).contains(&m) {
        return None;
    }
    let m = m.clamp(1, miy);
    let dim = cal_days_in_month(c, y, m);
    if reject && !(1..=dim).contains(&d) {
        return None;
    }
    let d = d.clamp(1, dim);
    Some(epoch_days_to_iso(cal_to_epoch_days(c, y, m, d)))
}

/// `CalendarDateAdd`'s year+month step in calendar space, with the day clamped
/// (or rejected) into the destination month. Weeks/days are left to the caller,
/// which adds them as exact epoch days.
pub(crate) fn cal_add_year_month(
    c: Cal,
    y: i64,
    m: i64,
    d: i64,
    add_y: i64,
    add_m: i64,
    reject: bool,
) -> Option<(i64, i64, i64)> {
    let miy = cal_months_in_year(c, y);
    // Every calendar here has a fixed month count, so month overflow rolls
    // through the year with plain division (no lunisolar leap-month walk).
    let total = (y + add_y) * miy + (m - 1) + add_m;
    let ny = total.div_euclid(miy);
    let nm = total.rem_euclid(miy) + 1;
    let dim = cal_days_in_month(c, ny, nm);
    if reject && d > dim {
        return None;
    }
    Some((ny, nm, d.min(dim)))
}

/// `CalendarDateUntil`: the years/months/weeks/days from `d1` to `d2`, both
/// given as ISO dates, measured in `c`.
///
/// The month count is the largest number of whole months that does not
/// *surpass* `d2`, where "surpass" compares the UNCLAMPED calendar triple
/// (y1, m1 + Δ, d1) against `d2`'s — so 1970-01-29 → 1970-02-28 is 30 days, not
/// one month: "Jan 29 + 1 month" is the (nonexistent) Feb 29, which is already
/// past Feb 28. Clamping the probe instead would call it a whole month and then
/// report zero days, which is what test262's `wrapping-at-end-of-month-*`
/// tests exist to rule out.
pub(crate) fn cal_difference_date(
    c: Cal,
    d1: (i64, i64, i64),
    d2: (i64, i64, i64),
    largest: &str,
) -> [i64; 4] {
    let (e1, e2) = (
        iso_to_epoch_days(d1.0, d1.1, d1.2),
        iso_to_epoch_days(d2.0, d2.1, d2.2),
    );
    if largest == "day" || largest == "week" {
        let mut days = e2 - e1;
        let mut weeks = 0;
        if largest == "week" {
            weeks = days / 7;
            days %= 7;
        }
        return [0, 0, weeks, days];
    }
    let sign = (e2 > e1) as i64 - (e2 < e1) as i64;
    if sign == 0 {
        return [0, 0, 0, 0];
    }
    let (y1, m1, dd1) = cal_from_epoch_days(c, e1);
    let (y2, m2, dd2) = cal_from_epoch_days(c, e2);
    let miy = cal_months_in_year(c, y1);
    // Whole months from (y1,m1) to (y2,m2), then one step back if the day of
    // month already puts the probe past d2.
    let mut total = (y2 - y1) * miy + (m2 - m1);
    let surpasses = if sign > 0 { dd1 > dd2 } else { dd1 < dd2 };
    if surpasses {
        total -= sign;
    }
    let years = if largest == "year" { total / miy } else { 0 };
    let months = total - years * miy;
    let (ay, am, ad) = cal_add_year_month(c, y1, m1, dd1, 0, total, false).unwrap();
    let days = e2 - cal_to_epoch_days(c, ay, am, ad);
    [years, months, 0, days]
}
