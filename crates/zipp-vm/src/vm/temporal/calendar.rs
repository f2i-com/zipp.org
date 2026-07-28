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
//! data. `hebrew` qualifies despite being lunisolar: since Hillel II its months
//! have been fixed by the 19-year Metonic cycle plus the four dechiyot
//! (postponement rules), so a Hebrew date is pure integer arithmetic on the
//! mean molad — no observation and no tables. `chinese`/`dangi` do NOT qualify
//! (true new moons and solar terms), nor do the observational islamic variants
//! (islamic-umalqura and friends): they need month-length tables or new-moon
//! computation we do not have, and a fake answer would be worse than the
//! honest `RangeError`.
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
    Persian = 10,
    Indian = 11,
    Hebrew = 12,
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
            10 => Cal::Persian,
            11 => Cal::Indian,
            12 => Cal::Hebrew,
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
            Cal::Persian => "persian",
            Cal::Indian => "indian",
            Cal::Hebrew => "hebrew",
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
        "persian" => Cal::Persian,
        "indian" => Cal::Indian,
        "hebrew" => Cal::Hebrew,
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

// ── Persian (Solar Hijri) helpers ───────────────────────────────────────────
//
// The 33-year ARITHMETIC cycle ICU4C uses, not an astronomical vernal-equinox
// computation. That is not an approximation of the civil Iranian calendar over
// the range anyone checks: the leap years and Nowruz dates it produces agree
// exactly with the Iranian calendar authority's published table for 1206–1498
// AP, which is what `inLeapYear/persian-calendar-authority.js` and
// `PlainDate/from/persian-new-year-dates.js` pin.

/// Epoch day of Persian 1-01-01 (R.D. 226895 = 622-03-22 proleptic Gregorian).
const PERSIAN_EPOCH: i64 = 226_895 - RD_AT_EPOCH;

/// Whether Persian year `y` is a leap year. Eight leap years per 33: seven
/// 4-year gaps then one of 5.
fn persian_leap(y: i64) -> bool {
    (25 * y + 11).rem_euclid(33) < 8
}

/// Epoch day of Persian `y`-01-01 (Nowruz). 365 days a year plus the 8
/// intercalary days the 33-year cycle has inserted so far — the same cycle
/// `persian_leap` reads, since `floor((8y+29)/33) - floor((8y+21)/33)` is 1
/// exactly when `(25y+11) mod 33 < 8`.
fn persian_year_start(y: i64) -> i64 {
    PERSIAN_EPOCH + 365 * (y - 1) + (8 * y + 21).div_euclid(33)
}

// ── Indian national (Saka) helpers ──────────────────────────────────────────

/// Whether Indian year `y` is a leap year. The 1957 reform defined the calendar
/// to keep step with the Gregorian one, so a year is leap exactly when the
/// Gregorian year its Chaitra 1 falls in is.
fn indian_leap(y: i64) -> bool {
    super::super::helpers_datetime::is_leap_year(y + 78)
}

/// Epoch day of Indian `y`-01-01 (1 Chaitra): 22 March of Gregorian y+78,
/// pulled back to 21 March when that Gregorian year is a leap year (so the
/// year always ends on the Gregorian 21 March that precedes the next Chaitra 1).
fn indian_year_start(y: i64) -> i64 {
    iso_to_epoch_days(y + 78, 3, if indian_leap(y) { 21 } else { 22 })
}

// ── Hebrew helpers ──────────────────────────────────────────────────────────
//
// The Hebrew calendar has been PURELY ARITHMETIC since Hillel II: a year is
// leap (13 months) on the 19-year Metonic cycle, and Rosh Hashanah is the mean
// molad of Tishri moved by the four dechiyot (postponements). Nothing here is
// observational and nothing needs CLDR, so it belongs in this file — unlike
// chinese/dangi, which need true new moons.
//
// Formulas are Reingold & Dershowitz's `hebrew-calendar-elapsed-days` /
// `hebrew-year-length-correction` / `hebrew-new-year`.

/// Epoch day of 1 Tishri 1 A.M. (R.D. -1373427 = -3760-09-07 proleptic Julian).
const HEBREW_EPOCH: i64 = -1_373_427 - RD_AT_EPOCH;

/// 19 Hebrew years hold 235 lunar months, 7 of which are the intercalary Adar I;
/// the leap years of a cycle are 3, 6, 8, 11, 14, 17 and 19.
fn hebrew_leap(y: i64) -> bool {
    (7 * y + 1).rem_euclid(19) < 7
}

/// Lunar months from the epoch to 1 Tishri of year `y` — 12 per common year plus
/// one per leap year, which `(7n+1)/19` counts exactly. Doubles as the global
/// month index, so month arithmetic across years of different length is a
/// subtraction rather than a walk.
fn hebrew_months_before(y: i64) -> i64 {
    (235 * y - 234).div_euclid(19)
}

/// Days from the epoch to the MEAN molad of Tishri of year `y`, already moved by
/// dechiya #1 (lo ADU rosh: Rosh Hashanah may not fall on Sun/Wed/Fri) and #2
/// (molad zaken, folded into the 25920-part division below).
fn hebrew_elapsed_days(y: i64) -> i64 {
    let months = hebrew_months_before(y);
    // One mean lunation is 29d 12h 793p; 12h 793p = 13753 parts of 25920 per day.
    let parts = 12084 + 13753 * months;
    let day = 29 * months + parts.div_euclid(25920);
    if (3 * (day + 1)).rem_euclid(7) < 3 { day + 1 } else { day }
}

/// Epoch day of 1 Tishri of Hebrew year `y`, with dechiyot #3 and #4: a year
/// that would otherwise run 356 days (GaTaRaD) or follow a 382-day one (BeTUTaKPaT)
/// has its start pushed out, which is what keeps every year length in
/// {353, 354, 355, 383, 384, 385}.
fn hebrew_year_start(y: i64) -> i64 {
    let e = hebrew_elapsed_days(y);
    let delay = if hebrew_elapsed_days(y + 1) - e == 356 {
        2
    } else if e - hebrew_elapsed_days(y - 1) == 382 {
        1
    } else {
        0
    };
    HEBREW_EPOCH + e + delay
}

/// Days in Hebrew year `y`: 353/354/355 (common) or 383/384/385 (leap).
fn hebrew_year_len(y: i64) -> i64 {
    hebrew_year_start(y + 1) - hebrew_year_start(y)
}

/// Days in ordinal month `m` (1 = Tishri) of Hebrew year `y`.
fn hebrew_days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 => 30, // Tishri
        // Heshvan gains its 30th day only in a "complete" year (355/385), and
        // Kislev loses its 30th only in a "deficient" one (353/383) — which is
        // exactly what the last digit of the year length records.
        2 => {
            if hebrew_year_len(y) % 10 == 5 {
                30
            } else {
                29
            }
        }
        3 => {
            if hebrew_year_len(y) % 10 == 3 {
                29
            } else {
                30
            }
        }
        // From Tevet on the months simply alternate 29/30 …
        _ if !hebrew_leap(y) => {
            if m % 2 == 0 {
                29
            } else {
                30
            }
        }
        // … except that a leap year splits Adar into a 30-day Adar I (ordinal 6)
        // and a 29-day Adar II (ordinal 7), which flips the parity after it.
        4 => 29,
        5 | 6 => 30,
        _ => {
            if m % 2 == 1 {
                29
            } else {
                30
            }
        }
    }
}

// ── The calendar interface ──────────────────────────────────────────────────

/// `CalendarDateMonthsInYear`. Year-dependent for `hebrew` (a leap year inserts
/// Adar I); 13 for the Coptic-structured calendars, 12 for everything else.
pub(crate) fn cal_months_in_year(c: Cal, y: i64) -> i64 {
    if c == Cal::Hebrew {
        return if hebrew_leap(y) { 13 } else { 12 };
    }
    if c.coptic_like().is_some() {
        13
    } else {
        12
    }
}

/// The greatest `monthsInYear` this calendar ever has — the year-independent
/// bound a field check can use before a year is known.
pub(crate) fn cal_max_months(c: Cal) -> i64 {
    if c == Cal::Hebrew || c.coptic_like().is_some() {
        13
    } else {
        12
    }
}

/// Months from a fixed origin to the start of calendar year `y`. Subtracting two
/// of these gives the exact month distance even when the years in between differ
/// in length, so month arithmetic never has to walk year by year.
pub(crate) fn cal_months_before_year(c: Cal, y: i64) -> i64 {
    if c == Cal::Hebrew {
        return hebrew_months_before(y);
    }
    y * cal_months_in_year(c, y)
}

/// The global (0-based) index of calendar month `y`-`m`. `m` need not be inside
/// `1..=monthsInYear`; the index simply rolls, which is what lets the difference
/// and addition algorithms probe past a year boundary.
pub(crate) fn cal_month_index(c: Cal, y: i64, m: i64) -> i64 {
    cal_months_before_year(c, y) + m - 1
}

/// The inverse of [`cal_month_index`].
pub(crate) fn cal_month_from_index(c: Cal, idx: i64) -> (i64, i64) {
    if c == Cal::Hebrew {
        // months_before(y) = floor((235y-234)/19) <= idx  ⟺  y <= (19·idx+252)/235.
        let mut y = (19 * idx + 252).div_euclid(235);
        while hebrew_months_before(y) > idx {
            y -= 1;
        }
        while hebrew_months_before(y + 1) <= idx {
            y += 1;
        }
        return (y, idx - hebrew_months_before(y) + 1);
    }
    let miy = cal_max_months(c);
    (idx.div_euclid(miy), idx.rem_euclid(miy) + 1)
}

/// `CalendarISOToDate`'s `[[MonthCode]]`, as (number, is-leap-month). Only
/// `hebrew` ever returns a leap code: its Adar I is `M05L`, sitting between
/// Shevat (`M05`) and the Adar that a common year keeps (`M06`), so every month
/// AFTER it has an ordinal one higher than its code.
pub(crate) fn cal_month_code(c: Cal, y: i64, m: i64) -> (i64, bool) {
    if c == Cal::Hebrew && hebrew_leap(y) {
        return match m {
            6 => (5, true),
            _ if m > 6 => (m - 1, false),
            _ => (m, false),
        };
    }
    (m, false)
}

/// Whether a well-formed month code is one this calendar has in SOME year — the
/// year-independent half of `CalendarResolveFields`' monthCode check. A code that
/// fails here is a RangeError under either overflow mode ("M13", "M02L"); one
/// that passes here but is missing from the requested year (hebrew "M05L" in a
/// common year) is subject to `overflow` instead.
pub(crate) fn cal_month_code_valid(c: Cal, num: i64, leap: bool) -> bool {
    if leap {
        // Adar I is the only leap month any implemented calendar has.
        return c == Cal::Hebrew && num == 5;
    }
    // A 13th ORDINARY month exists only in the coptic-structured calendars;
    // hebrew's 13th month is Adar I, which is the leap code M05L, not "M13".
    let last = if c.coptic_like().is_some() { 13 } else { 12 };
    (1..=last).contains(&num)
}

/// The ordinal month a month code names in calendar year `y`, or `None` when
/// that year does not have it (hebrew "M05L" in a common year).
pub(crate) fn cal_month_of_code(c: Cal, y: i64, num: i64, leap: bool) -> Option<i64> {
    if c == Cal::Hebrew {
        return if hebrew_leap(y) {
            if leap {
                (num == 5).then_some(6)
            } else if num <= 5 {
                Some(num)
            } else {
                Some(num + 1)
            }
        } else if leap {
            None
        } else {
            Some(num)
        };
    }
    (!leap && num <= cal_months_in_year(c, y)).then_some(num)
}

/// The `monthCode` string of an ordinal month: "M05" for an ordinary month,
/// "M05L" for a leap one.
pub(crate) fn month_code_string(c: Cal, y: i64, m: i64) -> String {
    let (num, leap) = cal_month_code(c, y, m);
    format!("M{num:02}{}", if leap { "L" } else { "" })
}

/// A month as `CalendarResolveFields` sees it BEFORE the calendar year is
/// resolved: either the ordinal `month` field or a `monthCode`. The two mean the
/// same thing in every fixed-month calendar, but not in a leap-month one — the
/// distinction has to survive until the year is known, because `M06` is ordinal
/// 6 in a common Hebrew year and ordinal 7 in a leap one.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum MonthRef {
    Ordinal(i64),
    Code(i64, bool),
}

impl MonthRef {
    /// The month code of an ordinal month — what a `with`/`add` carries forward
    /// when the caller did not supply a month of its own.
    pub(crate) fn of(c: Cal, y: i64, m: i64) -> MonthRef {
        let (num, leap) = cal_month_code(c, y, m);
        MonthRef::Code(num, leap)
    }

    /// The ordinal month this names in calendar year `y`. `None` only when
    /// `reject` and the year has no such month (hebrew "M05L" in a common year);
    /// `constrain` collapses that onto the neighbouring month instead. An
    /// ordinal is passed through unregulated — `cal_date_to_iso` bounds it.
    pub(crate) fn ordinal(self, c: Cal, y: i64, reject: bool) -> Option<i64> {
        match self {
            MonthRef::Ordinal(n) => Some(n),
            MonthRef::Code(num, leap) => match cal_month_of_code(c, y, num, leap) {
                Some(m) => Some(m),
                None if reject => None,
                None => Some(cal_month_of_code_constrain(c, y, num, leap)),
            },
        }
    }

    /// The value the `< 1` field-preparation floor applies to. A month code is
    /// well-formed by construction, so only a numeric `month` can be below 1.
    pub(crate) fn floor(self) -> i64 {
        match self {
            MonthRef::Ordinal(n) => n,
            MonthRef::Code(..) => 1,
        }
    }
}

/// `CalendarResolveFields`: a bag giving BOTH `month` and `monthCode` must have
/// them name the same month OF THAT YEAR — which is not "the same number" in a
/// leap-month calendar (`{ year: 5784, monthCode: "M06", month: 7 }` agrees,
/// per `PlainMonthDay/from/calendarresolvefields-error-ordering-hebrew.js`).
pub(crate) fn cal_month_fields_agree(c: Cal, y: i64, m: MonthRef, num: Option<i64>) -> bool {
    match (m, num) {
        (MonthRef::Code(n, leap), Some(k)) => cal_month_of_code(c, y, n, leap) == Some(k),
        _ => true,
    }
}

/// `overflow: "constrain"` for a month code the year does not have: the leap
/// month `M{n}L` sits immediately after `M{n}`, so it collapses onto whatever
/// month occupies that slot — for hebrew, Adar I becomes the common year's Adar
/// (`M05L` → `M06`), which `PlainYearMonth/from/reference-day-hebrew.js` pins.
pub(crate) fn cal_month_of_code_constrain(c: Cal, y: i64, num: i64, leap: bool) -> i64 {
    if let Some(m) = cal_month_of_code(c, y, num, leap) {
        return m;
    }
    let miy = cal_months_in_year(c, y);
    cal_month_of_code(c, y, num, false).map_or(miy, |m| m + 1).clamp(1, miy)
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
    if c == Cal::Persian {
        // Six 31-day months, five 30-day, then a 29-day (30 in a leap year) Esfand.
        return if m <= 6 {
            31
        } else if m <= 11 {
            30
        } else if persian_leap(y) {
            30
        } else {
            29
        };
    }
    if c == Cal::Hebrew {
        return hebrew_days_in_month(y, m);
    }
    if c == Cal::Indian {
        // Chaitra is 30 days (31 in a leap year), then five 31-day months and
        // six 30-day ones.
        return if m == 1 {
            if indian_leap(y) {
                31
            } else {
                30
            }
        } else if m <= 6 {
            31
        } else {
            30
        };
    }
    super::super::helpers_datetime::days_in_month(y, m)
}

/// The greatest length the month with this CODE ever has, searching back over a
/// full leap cycle from `y` — what `overflow: "constrain"` clamps a PlainMonthDay
/// day to, since its reference year is chosen afterwards. Keyed on the code, not
/// the ordinal, because in a leap-month calendar one ordinal names two different
/// months depending on the year.
pub(crate) fn cal_month_code_max_days(c: Cal, y: i64, num: i64, leap: bool) -> i64 {
    // 40 years covers the 4-year Coptic/Gregorian/Indian cycle, the 30-year
    // tabular-Islamic one, the 33-year Persian one (whose longest gap between
    // leap years is 5) and the 19-year Hebrew Metonic cycle.
    (0..40)
        .filter_map(|k| {
            cal_month_of_code(c, y - k, num, leap).map(|m| cal_days_in_month(c, y - k, m))
        })
        .max()
        .unwrap_or(0)
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
    if c == Cal::Persian {
        return persian_leap(y);
    }
    if c == Cal::Indian {
        return indian_leap(y);
    }
    if c == Cal::Hebrew {
        return hebrew_leap(y);
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
    if c == Cal::Persian || c == Cal::Indian {
        return if cal_in_leap_year(c, y) { 366 } else { 365 };
    }
    if c == Cal::Hebrew {
        return hebrew_year_len(y);
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
    if c == Cal::Persian {
        // 31 days each precede months 2..=7, then 30 each.
        let before = if m <= 7 { 31 * (m - 1) } else { 186 + 30 * (m - 7) };
        return persian_year_start(y) + before + d - 1;
    }
    if c == Cal::Indian {
        // Chaitra's own (year-dependent) length, then five 31s and up to six 30s.
        let before = if m <= 1 {
            31 * (m - 1)
        } else {
            cal_days_in_month(c, y, 1) + 31 * (m - 2).min(5) + 30 * (m - 7).max(0)
        };
        return indian_year_start(y) + before + d - 1;
    }
    if c == Cal::Hebrew {
        // No closed form for the month offsets (the first three month lengths
        // depend on the year's kevi'ah), so sum them — after normalising a month
        // that has rolled out of the year, which the difference probes rely on.
        let (ry, rm) = cal_month_from_index(c, cal_month_index(c, y, m));
        let mut ed = hebrew_year_start(ry);
        for k in 1..rm {
            ed += hebrew_days_in_month(ry, k);
        }
        return ed + d - 1;
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
    } else if c == Cal::Persian {
        // 33 Persian years are exactly 12053 days.
        (33 * (ed - PERSIAN_EPOCH) - 29).div_euclid(12053) + 1
    } else if c == Cal::Indian {
        // The Indian year `y` opens in ISO year y+78 (late March), so the ISO
        // year is the estimate and the loop below steps back a Jan–Mar date.
        epoch_days_to_iso(ed).0 - 78
    } else if c == Cal::Hebrew {
        // Mean Hebrew year = (235/19)·(29d 12h 793p) = 179876755/492480 days.
        ((ed - HEBREW_EPOCH) * 492_480).div_euclid(179_876_755) + 1
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
        // Single proleptic eras: the era year IS the arithmetic year, negative
        // values included (`from/non-positive-single-era-year.js`).
        Cal::Persian => ("ap", y),
        Cal::Indian => ("shaka", y),
        Cal::Hebrew => ("am", y),
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
        Cal::Persian => (era == "ap").then_some(era_year),
        Cal::Indian => (era == "shaka").then_some(era_year),
        Cal::Hebrew => (era == "am").then_some(era_year),
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

/// `CalendarDateAdd`'s YEAR step alone: shift the year keeping the MONTH CODE,
/// not the ordinal. For every fixed-month calendar the two are the same thing,
/// but in a leap-month calendar they part company — Adar II is `M06`, ordinal 7
/// in a leap year and ordinal 6 in a common one, and `add(1 year)` must land on
/// the Adar, not on Nisan (`PlainDate/prototype/add/leap-months-hebrew.js`).
///
/// `None` under `reject` when the destination year has no such month, e.g.
/// Adar I (`M05L`) + 1 year into a common year. The third component records
/// whether the month HAD to be constrained, which the difference algorithm needs.
fn cal_add_years(c: Cal, y: i64, m: i64, add_y: i64, reject: bool) -> Option<(i64, i64, bool)> {
    let (num, leap) = cal_month_code(c, y, m);
    let ny = y + add_y;
    match cal_month_of_code(c, ny, num, leap) {
        Some(nm) => Some((ny, nm, false)),
        None if reject => None,
        None => Some((ny, cal_month_of_code_constrain(c, ny, num, leap), true)),
    }
}

/// `CalendarDateAdd`'s year+month step in calendar space, with the day clamped
/// (or rejected) into the destination month. Weeks/days are left to the caller,
/// which adds them as exact epoch days.
///
/// Years go first (by month code), then months as ordinal steps through years
/// that may hold 12 or 13 of them — `cal_month_index` makes that a single add.
/// `m` must already be a real month of `y`: the year step reads its month CODE,
/// which an out-of-range ordinal does not have. (Every caller takes it from
/// `cal_from_iso`/`cal_from_epoch_days`, so it always is.)
pub(crate) fn cal_add_year_month(
    c: Cal,
    y: i64,
    m: i64,
    d: i64,
    add_y: i64,
    add_m: i64,
    reject: bool,
) -> Option<(i64, i64, i64)> {
    let (iy, im, _) = cal_add_years(c, y, m, add_y, reject)?;
    let (ny, nm) = cal_month_from_index(c, cal_month_index(c, iy, im) + add_m);
    let dim = cal_days_in_month(c, ny, nm);
    if reject && d > dim {
        return None;
    }
    Some((ny, nm, d.min(dim)))
}

/// `CalendarDateUntil`'s whole-YEAR step, shared by the date difference and
/// PlainYearMonth's (which has no day, so it passes 1 for both). Returns
/// `(years, anchor year, anchor month, months spanned by the NEXT year)` — the
/// last is the denominator a fractional-year rounding needs, and is NOT simply
/// `monthsInYear` in a calendar whose years differ in length.
pub(crate) fn cal_until_year_split(
    c: Cal,
    one: (i64, i64, i64),
    two: (i64, i64, i64),
) -> (i64, i64, i64, i64) {
    let (y1, m1, dd1) = one;
    let (y2, m2, dd2) = two;
    // Position of a probe for the surpass test: (month index, UNCLAMPED day, and
    // a final tie-break for a CONSTRAINED month). "M05L in a year that has none"
    // is not quite Adar — it is the gap just before it — so on an exact tie it
    // ranks earlier, and that is what decides whether the year counts: Hebrew
    // Adar I + 1 year onto Adar IS a whole year, while −1 year onto the same
    // Adar is not (`since/leap-months-hebrew.js`, "M05L-M06 backwards is -1y"
    // against "M05L-M06 is 12mo not 1y"). The tie-break is LAST so it cannot
    // mask an unclamped day that really has run past the month
    // (`wrapping-at-end-of-month-hebrew.js`: 30 Adar I to 29 Adar is not a year).
    let key = |y: i64, m: i64, constrained: bool, d: i64| {
        (cal_month_index(c, y, m), d, -(constrained as i64))
    };
    let e1 = cal_to_epoch_days(c, y1, m1, dd1);
    let e2 = cal_to_epoch_days(c, y2, m2, dd2);
    let sign = (e2 > e1) as i64 - (e2 < e1) as i64;
    let mut years = if sign == 0 { 0 } else { y2 - y1 };
    if years != 0 {
        // At most one step back is ever needed: the probe's year is exactly
        // y1+years, so years-sign already lands strictly before d2's year.
        let (py, pm, pc) = cal_add_years(c, y1, m1, years, false).unwrap();
        let (pk, tk) = (key(py, pm, pc, dd1), key(y2, m2, false, dd2));
        if (sign > 0 && pk > tk) || (sign < 0 && pk < tk) {
            years -= sign;
        }
    }
    let (ay, am, _) = cal_add_years(c, y1, m1, years, false).unwrap();
    // One more year in the direction of travel: the months it spans are what a
    // fractional year is measured against.
    let step = if sign == 0 { 1 } else { sign };
    let (ny, nm, _) = cal_add_years(c, y1, m1, years + step, false).unwrap();
    let span = cal_month_index(c, ny, nm) - cal_month_index(c, ay, am);
    (years, ay, am, if span == 0 { cal_months_in_year(c, ay) } else { span.abs() })
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
    // Whole YEARS first (largestUnit "year" only): the most that can be added by
    // month code without passing d2. The probe keeps d1's UNCLAMPED day for the
    // same reason the month probe does — a 30 Kislev that lands in a 29-day
    // Kislev has NOT completed the year.
    let (mut years, mut ay, mut am) = (0, y1, m1);
    if largest == "year" {
        let s = cal_until_year_split(c, (y1, m1, dd1), (y2, m2, dd2));
        years = s.0;
        ay = s.1;
        am = s.2;
    }
    // Then whole MONTHS from that anchor, as ordinal steps (a month index
    // difference, so years of 12 and 13 months both count correctly), stepping
    // back once if the day of month already puts the probe past d2.
    let mut months = cal_month_index(c, y2, m2) - cal_month_index(c, ay, am);
    if if sign > 0 { dd1 > dd2 } else { dd1 < dd2 } {
        months -= sign;
    }
    let (fy, fm, fd) = cal_add_year_month(c, ay, am, dd1, 0, months, false).unwrap();
    let days = e2 - cal_to_epoch_days(c, fy, fm, fd);
    [years, months, 0, days]
}
