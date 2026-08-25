#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;
use crate::vm::*;
use crate::vm::{cldr_en, dtf_pattern};

/// CreateDateTimeFormat's `required`/`defaults` pair. `required` decides which
/// options clear needDefaults (and which style is outright rejected); `defaults`
/// decides which components are filled in when none did.
///
/// Every entry point picks its own pair: `Intl.DateTimeFormat` is (any, date),
/// `Date.prototype.toLocaleString` is (any, all), `toLocaleDateString` is
/// (date, date), and each `Temporal.*.prototype.toLocaleString` uses the group
/// its own type has.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DtfDefaults {
    /// (any, date) — the `Intl.DateTimeFormat` constructor itself.
    Standard,
    /// (date, date) — toLocaleDateString, PlainDate, PlainYearMonth, PlainMonthDay.
    Date,
    /// (time, time) — toLocaleTimeString, PlainTime.
    Time,
    /// (any, all) — Date.prototype.toLocaleString, PlainDateTime, Instant.
    All,
    /// (any, all + timeZoneName "short") — ZonedDateTime.
    Zoned,
}

impl DtfDefaults {
    /// (date half required, time half required) — which component groups clear
    /// needDefaults, and which style the formatter accepts at all.
    pub(crate) fn required(self) -> (bool, bool) {
        match self {
            DtfDefaults::Date => (true, false),
            DtfDefaults::Time => (false, true),
            _ => (true, true),
        }
    }

    /// (fill year/month/day, fill hour/minute/second, fill timeZoneName).
    pub(crate) fn defaults(self) -> (bool, bool, bool) {
        match self {
            DtfDefaults::Standard | DtfDefaults::Date => (true, false, false),
            DtfDefaults::Time => (false, true, false),
            DtfDefaults::All => (true, true, false),
            DtfDefaults::Zoned => (true, true, true),
        }
    }
}

/// The calendars and numbering systems this engine actually has data for. The
/// list is the SAME one `Intl.supportedValuesOf` reports, and DateTimeFormat /
/// NumberFormat resolve an option against it — a well-formed but unsupported
/// value (`{calendar: "bangla"}`, `-u-nu-adlm`) falls back to the default rather
/// than being echoed back, which is what the supportedValuesOf round-trip tests
/// and the future-calendar fallback tests require.
pub(crate) const AVAILABLE_CALENDARS: &[&str] = &[
    // ECMA-402 AvailableCalendars: the calendars for which this implementation
    // provides Intl.DateTimeFormat functionality. Sorted, as the spec requires.
    // Each one formats through vm/temporal's arithmetic with CLDR `en` names
    // (cldr_en::CAL_MONTHS / CAL_ERAS) — advertising one it cannot format would
    // be worse than a short list.
    "buddhist",
    "chinese",
    "coptic",
    "dangi",
    "ethioaa",
    "ethiopic",
    "gregory",
    "hebrew",
    "indian",
    "islamic-civil",
    "islamic-tbla",
    "islamic-umalqura",
    "iso8601",
    "japanese",
    "persian",
    "roc",
];
