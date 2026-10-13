//! Date/time locale data. Other Intl services retain their own locale sets.
//!
//! The generated `cldr_*` modules spell each list as a slice of `&str`, and a
//! slice of `&str` costs an eight-byte pointer and length per string in a
//! WebAssembly module on top of the text: 12,527 of them, 100 KB, for the
//! skeleton and interval tables and the calendar name lists. Those tables
//! are read here, at compile time, into [`Strs`]: one string per list with
//! each entry followed by U+001F. The generated sources and their generator
//! stay as they are; their literal slices are `const`s that nothing reads at
//! run time, so none of them is kept.
//!
//! The separator, not a length byte per entry, because it compresses: the
//! same lists with a length before each entry, or with the lengths gathered
//! ahead of the text, came out 2.4 KB larger after Brotli in the JavaScript
//! module. Finding it costs the pattern matchers about a nanosecond an entry
//! (native, against eleven for the matching itself).
use super::{cldr_ar_eg, cldr_de, cldr_dtf_en, cldr_ja, cldr_zh};

/// A list of strings stored end to end in one, each followed by U+001F, and
/// how many there are. Built at compile time from the generated slices (see
/// [`Packer`]); CLDR text never contains the separator, which the build
/// checks.
#[derive(Clone, Copy)]
pub(crate) struct Strs {
    text: &'static str,
    len: u32,
}

/// U+001F, the unit separator, after every entry of a [`Strs`].
const SEP: u8 = 0x1f;

impl Strs {
    /// The entries, in their generated order.
    pub(crate) fn iter(self) -> Entries {
        Entries(self.text)
    }

    /// Entry `i`, found from whichever end is nearer: the Japanese era
    /// lists run to 237 entries, and the current era is the last.
    pub(crate) fn get(self, i: usize) -> Option<&'static str> {
        let len = self.len();
        if i >= len {
            None
        } else if i < len / 2 {
            self.iter().nth(i)
        } else {
            self.iter().nth_back(len - 1 - i)
        }
    }

    /// The number of entries.
    pub(crate) fn len(self) -> usize {
        self.len as usize
    }

    /// The entries two at a time: a list generated from `&[(&str, &str)]`.
    pub(crate) fn pairs(self) -> PairEntries {
        PairEntries(self.iter())
    }

    /// The entries three at a time: a list generated from
    /// `&[(&str, &str, &str)]`.
    pub(crate) fn triples(self) -> TripleEntries {
        TripleEntries(self.iter())
    }
}

/// The entries of a [`Strs`]: the text up to each separator.
#[derive(Clone)]
pub(crate) struct Entries(&'static str);

impl Entries {
    #[inline]
    fn cut_next(&mut self) -> Option<&'static str> {
        let at = self.0.bytes().position(|b| b == SEP)?;
        let entry = self.0.get(..at)?;
        self.0 = self.0.get(at + 1..)?;
        Some(entry)
    }
}

impl Iterator for Entries {
    type Item = &'static str;

    #[inline]
    fn next(&mut self) -> Option<&'static str> {
        self.cut_next()
    }
}

/// [`Strs::pairs`].
pub(crate) struct PairEntries(Entries);

impl Iterator for PairEntries {
    type Item = (&'static str, &'static str);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        Some((self.0.cut_next()?, self.0.cut_next()?))
    }
}

/// [`Strs::triples`].
pub(crate) struct TripleEntries(Entries);

impl Iterator for TripleEntries {
    type Item = (&'static str, &'static str, &'static str);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        Some((self.0.cut_next()?, self.0.cut_next()?, self.0.cut_next()?))
    }
}

impl DoubleEndedIterator for Entries {
    #[inline]
    fn next_back(&mut self) -> Option<&'static str> {
        let body = self.0.strip_suffix(SEP as char)?;
        let start = body.bytes().rposition(|b| b == SEP).map_or(0, |p| p + 1);
        let entry = body.get(start..)?;
        self.0 = self.0.get(..start)?;
        Some(entry)
    }
}

/// One calendar's patterns as the generated `CAL_PATTERNS` spell them; read
/// only at compile time, into [`CalPatterns`].
#[derive(Clone, Copy)]
pub(crate) struct CalendarPatterns {
    pub id: &'static str,
    pub available_formats: &'static [(&'static str, &'static str)],
    pub interval_formats: &'static [(&'static str, &'static str, &'static str)],
    pub interval_fallback: &'static str,
    pub datetime_glue_at: [&'static str; 4],
}

/// One calendar's patterns: `available_formats` holds (skeleton, pattern)
/// pairs and `interval_formats` (skeleton, greatest-difference field,
/// pattern) triples.
#[derive(Clone, Copy)]
pub(crate) struct CalPatterns {
    pub id: &'static str,
    pub available_formats: Strs,
    pub interval_formats: Strs,
    pub interval_fallback: &'static str,
    pub datetime_glue_at: [&'static str; 4],
}

/// A calendar's month or era names: (calendar, wide, abbreviated, narrow).
pub(crate) type CalNames = (&'static str, Strs, Strs, Strs);

pub(crate) struct DateTimeLocale {
    pub months_wide: [&'static str; 12],
    pub months_abbr: [&'static str; 12],
    pub months_narrow: [&'static str; 12],
    pub months_sa_wide: [&'static str; 12],
    pub months_sa_abbr: [&'static str; 12],
    pub months_sa_narrow: [&'static str; 12],
    pub days_wide: [&'static str; 7],
    pub days_abbr: [&'static str; 7],
    pub days_short: [&'static str; 7],
    pub days_narrow: [&'static str; 7],
    pub eras_wide: [&'static str; 2],
    pub eras_abbr: [&'static str; 2],
    pub eras_narrow: [&'static str; 2],
    pub cal_months: &'static [CalNames],
    pub cal_eras: &'static [CalNames],
    pub cal_date_formats: &'static [(&'static str, [&'static str; 4])],
    pub day_periods: &'static [(&'static str, &'static str, &'static str, &'static str)],
    pub day_period_rules: &'static [(&'static str, i32, i32, i32)],
    pub date_formats: [&'static str; 4],
    pub time_formats: [&'static str; 4],
    pub datetime_glue_at: [&'static str; 4],
    /// (skeleton, pattern) pairs.
    pub available_formats: Strs,
    /// (field, glue, name) triples.
    pub append_items: Strs,
    /// (skeleton, greatest-difference field, pattern) triples.
    pub interval_formats: Strs,
    pub interval_fallback: &'static str,
    pub sym_decimal: &'static str,
    pub utc_long: &'static str,
    pub hour_cycle: &'static str,
    pub hour_cycle12: &'static str,
    pub numbering_system: &'static str,
    pub calendar_patterns: &'static [CalPatterns],
    pub cyclic_years: &'static [(&'static str, Strs)],
    pub leap_month_patterns: &'static [(&'static str, [&'static str; 4], [&'static str; 4])],
    pub cal_date_number_systems: &'static [(&'static str, [&'static str; 4])],
    pub japanese_first_year: &'static str,
    pub lunar_day_names: Strs,
}

impl DateTimeLocale {
    pub fn patterns(&self, calendar: &str) -> CalPatterns {
        self.calendar_patterns
            .iter()
            .find(|p| p.id == calendar)
            .copied()
            .unwrap_or(CalPatterns {
                id: "gregory",
                available_formats: self.available_formats,
                interval_formats: self.interval_formats,
                interval_fallback: self.interval_fallback,
                datetime_glue_at: self.datetime_glue_at,
            })
    }
}

// ── Compile-time packing. Every function below runs in const evaluation only.
// A table's lists are numbered; a list equal to an earlier one of its table
// is not written again but shares that one's bytes (the linker did the same
// for equal slices: most calendars' skeleton lists are the same as another
// calendar's). Each table is measured, written into a byte array of that
// size, and cut into `Strs` at the ranges the measuring recorded.

type Names = &'static [(&'static str, &'static [&'static str], &'static [&'static str], &'static [&'static str])];
type Pairs = &'static [(&'static str, &'static str)];
type Triples = &'static [(&'static str, &'static str, &'static str)];

/// The most lists one table may have (fifteen calendars, three lists each).
const MAX_LISTS: usize = 64;

/// Where each list of a table lies in its packed bytes, and their total.
type Ranges = ([(usize, usize); MAX_LISTS], usize);

/// Writes strings into `buf`, each followed by [`SEP`].
struct Packer<const N: usize> {
    buf: [u8; N],
    at: usize,
}

impl<const N: usize> Packer<N> {
    const fn new() -> Self {
        Packer { buf: [0; N], at: 0 }
    }

    const fn put(mut self, s: &str) -> Self {
        let b = s.as_bytes();
        let mut k = 0;
        while k < b.len() {
            assert!(b[k] != SEP, "CLDR text contains U+001F");
            self.buf[self.at] = b[k];
            self.at += 1;
            k += 1;
        }
        self.buf[self.at] = SEP;
        self.at += 1;
        self
    }

    const fn list(mut self, l: &[&str]) -> Self {
        let mut i = 0;
        while i < l.len() {
            self = self.put(l[i]);
            i += 1;
        }
        self
    }

    const fn pairs(mut self, l: &[(&str, &str)]) -> Self {
        let mut i = 0;
        while i < l.len() {
            self = self.put(l[i].0).put(l[i].1);
            i += 1;
        }
        self
    }

    const fn triples(mut self, l: &[(&str, &str, &str)]) -> Self {
        let mut i = 0;
        while i < l.len() {
            self = self.put(l[i].0).put(l[i].1).put(l[i].2);
            i += 1;
        }
        self
    }

    const fn done(self) -> [u8; N] {
        assert!(self.at == N);
        self.buf
    }
}

const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

const fn list_eq(a: &[&str], b: &[&str]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if !str_eq(a[i], b[i]) {
            return false;
        }
        i += 1;
    }
    true
}

const fn pairs_eq(a: &[(&str, &str)], b: &[(&str, &str)]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if !str_eq(a[i].0, b[i].0) || !str_eq(a[i].1, b[i].1) {
            return false;
        }
        i += 1;
    }
    true
}

const fn triples_eq(a: &[(&str, &str, &str)], b: &[(&str, &str, &str)]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if !str_eq(a[i].0, b[i].0) || !str_eq(a[i].1, b[i].1) || !str_eq(a[i].2, b[i].2) {
            return false;
        }
        i += 1;
    }
    true
}

const fn list_size(l: &[&str]) -> usize {
    let (mut n, mut i) = (0, 0);
    while i < l.len() {
        n += l[i].len() + 1;
        i += 1;
    }
    n
}

const fn pairs_size(l: &[(&str, &str)]) -> usize {
    let (mut n, mut i) = (0, 0);
    while i < l.len() {
        n += l[i].0.len() + l[i].1.len() + 2;
        i += 1;
    }
    n
}

const fn triples_size(l: &[(&str, &str, &str)]) -> usize {
    let (mut n, mut i) = (0, 0);
    while i < l.len() {
        n += l[i].0.len() + l[i].1.len() + l[i].2.len() + 3;
        i += 1;
    }
    n
}

/// `buf[range]` as a [`Strs`].
const fn cut(buf: &'static [u8], range: (usize, usize)) -> Strs {
    let (_, tail) = buf.split_at(range.0);
    let (part, _) = tail.split_at(range.1 - range.0);
    let (mut len, mut i) = (0, 0);
    while i < part.len() {
        if part[i] == SEP {
            len += 1;
        }
        i += 1;
    }
    match core::str::from_utf8(part) {
        Ok(text) => Strs { text, len },
        Err(_) => panic!("a packed list is not UTF-8"),
    }
}

// Calendar names (`CAL_MONTHS`, `CAL_ERAS`): list `k` is field `k % 3` (wide,
// abbreviated, narrow) of row `k / 3`. Japanese has its 237 eras twice over,
// wide and abbreviated being the same.

const fn names_list(t: Names, k: usize) -> &'static [&'static str] {
    match k % 3 {
        0 => t[k / 3].1,
        1 => t[k / 3].2,
        _ => t[k / 3].3,
    }
}

const fn names_first(t: Names, k: usize) -> usize {
    let mut j = 0;
    while j < k {
        if list_eq(names_list(t, j), names_list(t, k)) {
            return j;
        }
        j += 1;
    }
    k
}

const fn names_ranges(t: Names) -> Ranges {
    assert!(t.len() * 3 <= MAX_LISTS);
    let mut r = [(0, 0); MAX_LISTS];
    let (mut at, mut k) = (0, 0);
    while k < t.len() * 3 {
        let first = names_first(t, k);
        if first == k {
            r[k] = (at, at + list_size(names_list(t, k)));
            at = r[k].1;
        } else {
            r[k] = r[first];
        }
        k += 1;
    }
    (r, at)
}

const fn names_bytes<const N: usize>(t: Names) -> [u8; N] {
    let mut p = Packer::<N>::new();
    let mut k = 0;
    while k < t.len() * 3 {
        if names_first(t, k) == k {
            p = p.list(names_list(t, k));
        }
        k += 1;
    }
    p.done()
}

const fn names_rows<const R: usize>(t: Names, buf: &'static [u8]) -> [CalNames; R] {
    let (r, _) = names_ranges(t);
    let empty = Strs { text: "", len: 0 };
    let mut out = [("", empty, empty, empty); R];
    let mut i = 0;
    while i < R {
        out[i] = (t[i].0, cut(buf, r[3 * i]), cut(buf, r[3 * i + 1]), cut(buf, r[3 * i + 2]));
        i += 1;
    }
    out
}

// Skeleton and interval patterns: list 0 is the gregorian `AVAILABLE_FORMATS`
// and list 1 its `INTERVAL_FORMATS`; lists `2 + 2i` and `3 + 2i` are those of
// `CAL_PATTERNS[i]`. Even lists are pairs, odd ones triples.

const fn pattern_pairs(ga: Pairs, cp: &[CalendarPatterns], k: usize) -> Pairs {
    if k == 0 { ga } else { cp[(k - 2) / 2].available_formats }
}

const fn pattern_triples(gi: Triples, cp: &[CalendarPatterns], k: usize) -> Triples {
    if k == 1 { gi } else { cp[(k - 3) / 2].interval_formats }
}

const fn pattern_first(ga: Pairs, gi: Triples, cp: &[CalendarPatterns], k: usize) -> usize {
    let mut j = k % 2;
    while j < k {
        let same = if k % 2 == 0 {
            pairs_eq(pattern_pairs(ga, cp, j), pattern_pairs(ga, cp, k))
        } else {
            triples_eq(pattern_triples(gi, cp, j), pattern_triples(gi, cp, k))
        };
        if same {
            return j;
        }
        j += 2;
    }
    k
}

const fn pattern_ranges(ga: Pairs, gi: Triples, cp: &[CalendarPatterns]) -> Ranges {
    assert!(2 + cp.len() * 2 <= MAX_LISTS);
    let mut r = [(0, 0); MAX_LISTS];
    let (mut at, mut k) = (0, 0);
    while k < 2 + cp.len() * 2 {
        let first = pattern_first(ga, gi, cp, k);
        if first == k {
            let size = if k % 2 == 0 {
                pairs_size(pattern_pairs(ga, cp, k))
            } else {
                triples_size(pattern_triples(gi, cp, k))
            };
            r[k] = (at, at + size);
            at = r[k].1;
        } else {
            r[k] = r[first];
        }
        k += 1;
    }
    (r, at)
}

const fn pattern_bytes<const N: usize>(ga: Pairs, gi: Triples, cp: &[CalendarPatterns]) -> [u8; N] {
    let mut p = Packer::<N>::new();
    let mut k = 0;
    while k < 2 + cp.len() * 2 {
        if pattern_first(ga, gi, cp, k) == k {
            p = if k % 2 == 0 {
                p.pairs(pattern_pairs(ga, cp, k))
            } else {
                p.triples(pattern_triples(gi, cp, k))
            };
        }
        k += 1;
    }
    p.done()
}

const fn pattern_rows<const R: usize>(
    ga: Pairs,
    gi: Triples,
    cp: &[CalendarPatterns],
    buf: &'static [u8],
) -> [CalPatterns; R] {
    let (r, _) = pattern_ranges(ga, gi, cp);
    let empty = Strs { text: "", len: 0 };
    let mut out = [CalPatterns {
        id: "",
        available_formats: empty,
        interval_formats: empty,
        interval_fallback: "",
        datetime_glue_at: [""; 4],
    }; R];
    let mut i = 0;
    while i < R {
        out[i] = CalPatterns {
            id: cp[i].id,
            available_formats: cut(buf, r[2 + 2 * i]),
            interval_formats: cut(buf, r[3 + 2 * i]),
            interval_fallback: cp[i].interval_fallback,
            datetime_glue_at: cp[i].datetime_glue_at,
        };
        i += 1;
    }
    out
}

/// The gregorian list `k` (0 skeletons, 1 intervals) of a pattern table.
const fn pattern_list(ga: Pairs, gi: Triples, cp: &[CalendarPatterns], buf: &'static [u8], k: usize) -> Strs {
    cut(buf, pattern_ranges(ga, gi, cp).0[k])
}

// Cyclic year names, one list per row.

const fn cyclic_size(t: &[(&str, &[&str])]) -> usize {
    let (mut n, mut i) = (0, 0);
    while i < t.len() {
        n += list_size(t[i].1);
        i += 1;
    }
    n
}

const fn cyclic_bytes<const N: usize>(t: &[(&str, &[&str])]) -> [u8; N] {
    let mut p = Packer::<N>::new();
    let mut i = 0;
    while i < t.len() {
        p = p.list(t[i].1);
        i += 1;
    }
    p.done()
}

const fn cyclic_rows<const R: usize>(
    t: &[(&'static str, &[&str])],
    buf: &'static [u8],
) -> [(&'static str, Strs); R] {
    let mut out = [("", Strs { text: "", len: 0 }); R];
    let (mut at, mut i) = (0, 0);
    while i < R {
        let end = at + list_size(t[i].1);
        out[i] = (t[i].0, cut(buf, (at, end)));
        at = end;
        i += 1;
    }
    out
}

const fn list_bytes<const N: usize>(l: &[&str]) -> [u8; N] {
    Packer::<N>::new().list(l).done()
}

/// One generated table, packed: the first token names its shape.
macro_rules! packed {
    (list $src:expr) => {{
        const N: usize = list_size($src);
        static BUF: [u8; N] = list_bytes::<N>($src);
        cut(&BUF, (0, N))
    }};
    (names $src:expr) => {{
        const SRC: Names = $src;
        const N: usize = names_ranges(SRC).1;
        static BUF: [u8; N] = names_bytes::<N>(SRC);
        static ROWS: [CalNames; SRC.len()] = names_rows::<{ SRC.len() }>(SRC, &BUF);
        &ROWS
    }};
    (cyclic $src:expr) => {{
        const SRC: &[(&str, &[&str])] = $src;
        const N: usize = cyclic_size(SRC);
        static BUF: [u8; N] = cyclic_bytes::<N>(SRC);
        static ROWS: [(&str, Strs); SRC.len()] = cyclic_rows::<{ SRC.len() }>(SRC, &BUF);
        &ROWS
    }};
}

macro_rules! locale {
    ($module:ident) => {{
        // The gregorian skeletons and intervals and every calendar's, in one
        // table so that equal lists are stored once.
        const GA: Pairs = $module::AVAILABLE_FORMATS;
        const GI: Triples = $module::INTERVAL_FORMATS;
        const CP: &[CalendarPatterns] = $module::CAL_PATTERNS;
        const PN: usize = pattern_ranges(GA, GI, CP).1;
        static PBUF: [u8; PN] = pattern_bytes::<PN>(GA, GI, CP);
        static PROWS: [CalPatterns; CP.len()] = pattern_rows::<{ CP.len() }>(GA, GI, CP, &PBUF);
        const APPEND: Triples = $module::APPEND_ITEMS;
        const AN: usize = triples_size(APPEND);
        static ABUF: [u8; AN] = Packer::<AN>::new().triples(APPEND).done();
        DateTimeLocale {
            months_wide: $module::MONTHS_WIDE,
            months_abbr: $module::MONTHS_ABBR,
            months_narrow: $module::MONTHS_NARROW,
            months_sa_wide: $module::MONTHS_SA_WIDE,
            months_sa_abbr: $module::MONTHS_SA_ABBR,
            months_sa_narrow: $module::MONTHS_SA_NARROW,
            days_wide: $module::DAYS_WIDE,
            days_abbr: $module::DAYS_ABBR,
            days_short: $module::DAYS_SHORT,
            days_narrow: $module::DAYS_NARROW,
            eras_wide: $module::ERAS_WIDE,
            eras_abbr: $module::ERAS_ABBR,
            eras_narrow: $module::ERAS_NARROW,
            cal_months: packed!(names $module::CAL_MONTHS),
            cal_eras: packed!(names $module::CAL_ERAS),
            cal_date_formats: $module::CAL_DATE_FORMATS,
            day_periods: $module::DAY_PERIODS,
            day_period_rules: $module::DAY_PERIOD_RULES,
            date_formats: $module::DATE_FORMATS,
            time_formats: $module::TIME_FORMATS,
            datetime_glue_at: $module::DATETIME_GLUE_AT,
            available_formats: pattern_list(GA, GI, CP, &PBUF, 0),
            append_items: cut(&ABUF, (0, AN)),
            interval_formats: pattern_list(GA, GI, CP, &PBUF, 1),
            interval_fallback: $module::INTERVAL_FALLBACK,
            sym_decimal: $module::SYM_DECIMAL,
            utc_long: $module::UTC_LONG,
            hour_cycle: $module::DEFAULT_HOUR_CYCLE,
            hour_cycle12: $module::DEFAULT_HOUR_CYCLE12,
            numbering_system: $module::DEFAULT_NUMBERING_SYSTEM,
            calendar_patterns: &PROWS,
            cyclic_years: packed!(cyclic $module::CYCLIC_YEARS),
            leap_month_patterns: $module::LEAP_MONTH_PATTERNS,
            cal_date_number_systems: $module::CAL_DATE_NUMBER_SYSTEMS,
            japanese_first_year: $module::JAPANESE_FIRST_YEAR,
            lunar_day_names: packed!(list $module::LUNAR_DAY_NAMES),
        }
    }};
}

pub(crate) static EN: DateTimeLocale = locale!(cldr_dtf_en);
pub(crate) static DE: DateTimeLocale = locale!(cldr_de);
pub(crate) static JA: DateTimeLocale = locale!(cldr_ja);
pub(crate) static ZH: DateTimeLocale = locale!(cldr_zh);
pub(crate) static AR_EG: DateTimeLocale = locale!(cldr_ar_eg);

pub(crate) fn for_locale(locale: &str) -> &'static DateTimeLocale {
    match locale.split('-').next() {
        Some("de") => &DE,
        Some("ja") => &JA,
        Some("zh") => &ZH,
        Some("ar") if locale.starts_with("ar-EG") => &AR_EG,
        _ => &EN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same_list(what: &str, packed: Strs, generated: &[&str]) {
        let got: Vec<&str> = packed.iter().collect();
        assert_eq!(got, generated, "{what}");
        assert_eq!(packed.len(), generated.len(), "{what}: len");
        for (i, s) in generated.iter().enumerate() {
            assert_eq!(packed.get(i), Some(*s), "{what}[{i}]");
        }
        assert_eq!(packed.get(generated.len()), None, "{what}: past the end");
    }

    fn same_names(what: &str, packed: &[CalNames], generated: Names) {
        assert_eq!(packed.len(), generated.len(), "{what}: rows");
        for ((id, w, a, n), (gid, gw, ga, gn)) in packed.iter().zip(generated) {
            assert_eq!(id, gid, "{what}");
            same_list(&format!("{what} {id} wide"), *w, gw);
            same_list(&format!("{what} {id} abbr"), *a, ga);
            same_list(&format!("{what} {id} narrow"), *n, gn);
        }
    }

    fn same_pairs(what: &str, packed: Strs, generated: Pairs) {
        let got: Vec<(&str, &str)> = packed.pairs().collect();
        assert_eq!(got, generated, "{what}");
    }

    fn same_triples(what: &str, packed: Strs, generated: Triples) {
        let got: Vec<(&str, &str, &str)> = packed.triples().collect();
        assert_eq!(got, generated, "{what}");
    }

    macro_rules! check {
        ($loc:expr, $module:ident) => {{
            let loc: &DateTimeLocale = $loc;
            let m = stringify!($module);
            same_names(&format!("{m} CAL_MONTHS"), loc.cal_months, $module::CAL_MONTHS);
            same_names(&format!("{m} CAL_ERAS"), loc.cal_eras, $module::CAL_ERAS);
            same_pairs(&format!("{m} AVAILABLE_FORMATS"), loc.available_formats, $module::AVAILABLE_FORMATS);
            same_triples(&format!("{m} APPEND_ITEMS"), loc.append_items, $module::APPEND_ITEMS);
            same_triples(&format!("{m} INTERVAL_FORMATS"), loc.interval_formats, $module::INTERVAL_FORMATS);
            same_list(&format!("{m} LUNAR_DAY_NAMES"), loc.lunar_day_names, $module::LUNAR_DAY_NAMES);
            assert_eq!(loc.cyclic_years.len(), $module::CYCLIC_YEARS.len(), "{m} CYCLIC_YEARS");
            for ((id, names), (gid, gnames)) in loc.cyclic_years.iter().zip($module::CYCLIC_YEARS) {
                assert_eq!(id, gid, "{m} CYCLIC_YEARS");
                same_list(&format!("{m} CYCLIC_YEARS {id}"), *names, gnames);
            }
            assert_eq!(loc.calendar_patterns.len(), $module::CAL_PATTERNS.len(), "{m} CAL_PATTERNS");
            for (p, g) in loc.calendar_patterns.iter().zip($module::CAL_PATTERNS) {
                assert_eq!(p.id, g.id, "{m} CAL_PATTERNS");
                same_pairs(&format!("{m} {} available", g.id), p.available_formats, g.available_formats);
                same_triples(&format!("{m} {} interval", g.id), p.interval_formats, g.interval_formats);
                assert_eq!(p.interval_fallback, g.interval_fallback);
                assert_eq!(p.datetime_glue_at, g.datetime_glue_at);
                let found = loc.patterns(g.id);
                assert_eq!(found.id, g.id);
                same_pairs(&format!("{m} {} via patterns()", g.id), found.available_formats, g.available_formats);
            }
            let gregory = loc.patterns("gregory");
            same_pairs(&format!("{m} gregory"), gregory.available_formats, $module::AVAILABLE_FORMATS);
            same_triples(&format!("{m} gregory"), gregory.interval_formats, $module::INTERVAL_FORMATS);
        }};
    }

    /// Every packed list holds exactly what the generated slices hold, entry
    /// for entry, in every locale.
    #[test]
    fn packed_tables_match_the_generated_slices() {
        check!(&EN, cldr_dtf_en);
        check!(&DE, cldr_de);
        check!(&JA, cldr_ja);
        check!(&ZH, cldr_zh);
        check!(&AR_EG, cldr_ar_eg);
    }

    #[test]
    fn empty_entries_and_empty_lists_survive_packing() {
        const L: &[&str] = &["", "a", "", ""];
        let s = packed!(list L);
        same_list("empty entries", s, L);
        const E: &[&str] = &[];
        let e = packed!(list E);
        same_list("empty list", e, E);
        assert_eq!(e.pairs().count(), 0);
    }
}
