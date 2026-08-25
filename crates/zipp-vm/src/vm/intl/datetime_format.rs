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

impl<'p> Vm<'p> {
    /// HandleDateTimeValue's argument classification: `None` for an ordinary time
    /// value (a Number, a Date, anything ToNumber-able), `Some(k)` for a
    /// `Temporal.*` argument (k = the HeapObj::Temporal kind). formatRange pairs
    /// only equal classifications — mixing a Date with a PlainDate, or two
    /// different Temporal types, is a TypeError.
    pub(crate) fn dt_arg_kind(&self, v: Value) -> Option<u8> {
        if v.is_heap() {
            if let HeapObj::Temporal { kind, .. } = self.heap.get(v.heap_index()) {
                return Some(*kind);
            }
        }
        None
    }

    /// HandleDateTimeValue's calendar guard: a `Temporal.*` argument is rendered
    /// with the DateTimeFormat's own calendar, so its calendar must be either
    /// `iso8601` (which any calendar can render) or exactly the format's.
    /// Anything else is a RangeError — the formatter must not silently reinterpret
    /// a Japanese date as a Gregorian one.
    pub(crate) fn dtf_check_calendar(
        &mut self,
        resolved: u32,
        v: Value,
        name: &str,
    ) -> Result<(), Thrown> {
        // Only the calendar-BEARING Temporal types carry a calendar to clash.
        if !self
            .dt_arg_kind(v)
            .is_some_and(|k| matches!(k, 1 | 3 | 5 | 6 | 7))
        {
            return Ok(());
        }
        let cal = self.cal_of(v.heap_index());
        if cal == crate::vm::temporal::Cal::Iso {
            return Ok(());
        }
        let want = self.intl_slot(resolved, "calendar");
        let want = if want.is_heap() {
            self.heap
                .str_cow(want.heap_index())
                .map(|s| s.into_owned())
                .unwrap_or_default()
        } else {
            String::new()
        };
        if cal.id() == want {
            return Ok(());
        }
        Err(Thrown(format!(
            "RangeError: {name} cannot format a {} date with the {want} calendar",
            cal.id()
        )))
    }

    /// One bit per DateTimeFormat component (ECMA-402 Table 7 order).
    ///
    /// ECMA-402's Temporal integration gives one DateTimeFormat a *separate*
    /// resolved pattern per Temporal type ([[TemporalPlainDateFormat]] and
    /// friends): each is ToDateTimeOptions(originalOptions, ANY, <that type's
    /// group>) intersected with the fields that type actually has, and an empty
    /// result makes HandleDateTimeValue throw a TypeError. Masks express both
    /// halves — which parts print, and whether anything prints at all.
    pub(crate) const F_ERA: u16 = 1;
    pub(crate) const F_YEAR: u16 = 2;
    pub(crate) const F_MONTH: u16 = 4;
    pub(crate) const F_DAY: u16 = 8;
    pub(crate) const F_WEEKDAY: u16 = 16;
    pub(crate) const F_DAYPERIOD: u16 = 32;
    pub(crate) const F_HOUR: u16 = 64;
    pub(crate) const F_MINUTE: u16 = 128;
    pub(crate) const F_SECOND: u16 = 256;
    pub(crate) const F_FRAC: u16 = 512;
    pub(crate) const F_ZONE: u16 = 1024;
    const F_DATE: u16 = Self::F_ERA | Self::F_YEAR | Self::F_MONTH | Self::F_DAY | Self::F_WEEKDAY;
    const F_TIME: u16 =
        Self::F_DAYPERIOD | Self::F_HOUR | Self::F_MINUTE | Self::F_SECOND | Self::F_FRAC;

    /// (allowed, defaults) for a `dt_arg_kind` — the fields a value of that type
    /// can contribute, and the group ToDateTimeOptions fills in when the options
    /// named no component at all.
    fn temporal_fields(kind: u8) -> (u16, u16) {
        const YMD: u16 = Vm::F_YEAR | Vm::F_MONTH | Vm::F_DAY;
        const HMS: u16 = Vm::F_HOUR | Vm::F_MINUTE | Vm::F_SECOND;
        match kind {
            1 => (Self::F_DATE, YMD),                      // PlainDate
            2 => (Self::F_TIME, HMS),                      // PlainTime
            3 => (Self::F_DATE | Self::F_TIME, YMD | HMS), // PlainDateTime (no zone)
            // ZonedDateTime — reachable only from its own toLocaleString
            // (Intl.DateTimeFormat rejects the type outright), where the zone
            // name is part of the defaults group.
            7 => (
                Self::F_DATE | Self::F_TIME | Self::F_ZONE,
                YMD | HMS | Self::F_ZONE,
            ),
            5 => (
                Self::F_ERA | Self::F_YEAR | Self::F_MONTH,
                Self::F_YEAR | Self::F_MONTH,
            ),
            6 => (Self::F_MONTH | Self::F_DAY, Self::F_MONTH | Self::F_DAY),
            _ => (Self::F_DATE | Self::F_TIME | Self::F_ZONE, YMD | HMS), // Instant
        }
    }

    /// The components `resolved` names. `dateStyle`/`timeStyle` stand for whole
    /// groups; `full`/`long` time styles also carry the zone name, which is why
    /// a PlainDateTime formatted with `timeStyle: "long"` must print less than an
    /// Instant does.
    pub(crate) fn dtf_requested_fields(&self, resolved: u32) -> u16 {
        let has = |k: &str| self.intl_slot(resolved, k) != Value::UNDEFINED;
        let mut m = 0u16;
        for (name, bit) in [
            ("era", Self::F_ERA),
            ("year", Self::F_YEAR),
            ("month", Self::F_MONTH),
            ("day", Self::F_DAY),
            ("weekday", Self::F_WEEKDAY),
            ("dayPeriod", Self::F_DAYPERIOD),
            ("hour", Self::F_HOUR),
            ("minute", Self::F_MINUTE),
            ("second", Self::F_SECOND),
            ("fractionalSecondDigits", Self::F_FRAC),
            ("timeZoneName", Self::F_ZONE),
        ] {
            if has(name) {
                m |= bit;
            }
        }
        if has("dateStyle") {
            m |= Self::F_YEAR | Self::F_MONTH | Self::F_DAY;
            // CLDR's `en` full date pattern is `EEEE, MMMM d, y` — the only one
            // of the four that carries a weekday, so `dateStyle: "full"` is the
            // only style whose component set includes one ("Saturday, May 1,
            // 1886" against "May 1, 1886").
            if self.display(self.intl_slot(resolved, "dateStyle")) == "full" {
                m |= Self::F_WEEKDAY;
            }
        }
        if has("timeStyle") {
            m |= Self::F_HOUR | Self::F_MINUTE | Self::F_SECOND;
            let ts = self.display(self.intl_slot(resolved, "timeStyle"));
            if ts == "full" || ts == "long" {
                m |= Self::F_ZONE;
            }
        }
        m
    }

    /// HandleDateTimeValue's field resolution for one argument. Returns the
    /// components to print and whether the value is an ABSOLUTE time (a legacy
    /// Date/number or a Temporal.Instant, which renders in the formatter's time
    /// zone; a plain Temporal value renders its own wall clock, because the
    /// spec's local -> epoch -> local round trip through the same zone cancels).
    ///
    /// A `Temporal.*` value whose fields do not intersect the pattern at all is
    /// a TypeError (`temporal-objects-not-overlapping-options.js`): the per-type
    /// format record would be null.
    pub(crate) fn dtf_fields_for(
        &mut self,
        resolved: u32,
        v: Value,
        name: &str,
    ) -> Result<(u16, bool), Thrown> {
        let requested = self.dtf_requested_fields(resolved);
        let Some(kind) = self.dt_arg_kind(v) else {
            return Ok((requested, true));
        };
        if matches!(kind, 0 | 7) {
            return Ok((requested, true)); // Duration/ZonedDateTime: dtf_time_value rejects
        }
        self.dtf_fields_for_kind(resolved, kind, name)
    }

    /// `dtf_fields_for` with the Temporal kind supplied directly, so
    /// `Temporal.ZonedDateTime.prototype.toLocaleString` — the one caller for
    /// which a ZonedDateTime is a legal argument — can reach the same resolution.
    pub(crate) fn dtf_fields_for_kind(
        &mut self,
        resolved: u32,
        kind: u8,
        name: &str,
    ) -> Result<(u16, bool), Thrown> {
        let requested = self.dtf_requested_fields(resolved);
        let (allowed, defaults) = Self::temporal_fields(kind);
        // ToDateTimeOptions' needDefaults: only a Table-7 date/time COMPONENT
        // clears it — `era` and `timeZoneName` do not (which is why
        // `{timeZoneName: "long"}` still resolves to year/month/day), but a
        // dateStyle/timeStyle does.
        let clears = Self::F_YEAR
            | Self::F_MONTH
            | Self::F_DAY
            | Self::F_WEEKDAY
            | Self::F_DAYPERIOD
            | Self::F_HOUR
            | Self::F_MINUTE
            | Self::F_SECOND
            | Self::F_FRAC;
        let styled = self.intl_slot(resolved, "dateStyle") != Value::UNDEFINED
            || self.intl_slot(resolved, "timeStyle") != Value::UNDEFINED;
        // `@@dtfDefaulted` marks a year/month/day that ToDateTimeOptions supplied
        // rather than the caller — those must not clear needDefaults here.
        let defaulted = self.intl_slot(resolved, "@@dtfDefaulted") == Value::bool(true);
        let need_defaults = defaulted || (requested & clears == 0 && !styled);
        // ToDateTimeOptions ADDS the defaults group to the options; it does not
        // replace them. `{era: "narrow"}` therefore resolves to era + the date
        // defaults, so an Instant formatted by that formatter still prints its
        // era — which is what makes it agree with `Date.prototype.toLocaleString`
        // under the same options (`format/temporal-objects-format-with-era.js`).
        let effective = if need_defaults {
            requested | defaults
        } else {
            requested
        } & allowed;
        if effective == 0 {
            return Err(Thrown(format!(
                "TypeError: {name} options do not include any field this Temporal value has"
            )));
        }
        // Instant and ZonedDateTime are absolute times: they render THROUGH the
        // formatter's zone. Every plain type carries its own wall clock instead.
        Ok((effective, matches!(kind, 4 | 7)))
    }

    /// HandleDateTimeValue: ToNumber + TimeClip for an ordinary time value (an
    /// out-of-range or non-finite one is a RangeError, and the result is an
    /// integer so `format(-0.9)` and `format(0)` agree), or the epoch time of a
    /// `Temporal.*` argument read as a UTC wall clock. Temporal.ZonedDateTime is
    /// explicitly rejected by the spec (its own toLocaleString handles it), and a
    /// Temporal.Duration is not a date-time at all.
    pub(crate) fn dtf_time_value(&mut self, v: Value) -> Result<f64, Thrown> {
        if let Some(kind) = self.dt_arg_kind(v) {
            let idx = v.heap_index();
            let day_ms = |y: i64, m: i64, d: i64| iso_to_epoch_days(y, m, d) as f64 * 86_400_000.0;
            return match kind {
                7 => Err(Thrown(
                    "TypeError: Intl.DateTimeFormat does not support Temporal.ZonedDateTime; use toLocaleString()"
                        .into(),
                )),
                0 => Err(Thrown("TypeError: Temporal.Duration is not a date-time value".into())),
                1 => {
                    let (y, m, d) = self.plain_date_fields(idx).unwrap_or((1970, 1, 1));
                    Ok(day_ms(y, m, d))
                }
                2 => {
                    let f = self.plain_time_fields(idx).unwrap_or([0; 6]);
                    Ok((f[0] * 3_600_000 + f[1] * 60_000 + f[2] * 1000 + f[3]) as f64)
                }
                3 => {
                    let f = self.pdt_fields(idx).unwrap_or([0; 9]);
                    Ok(day_ms(f[0], f[1], f[2])
                        + (f[3] * 3_600_000 + f[4] * 60_000 + f[5] * 1000 + f[6]) as f64)
                }
                4 => {
                    let ns = self.instant_ns(idx).unwrap_or(0);
                    Ok((ns / 1_000_000) as f64)
                }
                // PlainYearMonth (y, m, refDay) / PlainMonthDay (refYear, m, d).
                5 => match self.heap.get(idx) {
                    HeapObj::Temporal { fields, .. } => {
                        Ok(day_ms(fields[0], fields[1], *fields.get(2).unwrap_or(&1)))
                    }
                    _ => Ok(0.0),
                },
                _ => match self.heap.get(idx) {
                    HeapObj::Temporal { fields, .. } => {
                        Ok(day_ms(fields[0], fields[1], *fields.get(2).unwrap_or(&1)))
                    }
                    _ => Ok(0.0),
                },
            };
        }
        // ToNumber then TimeClip. The full ToNumber: a plain object argument
        // ToPrimitive's first, so a throwing `valueOf` wins over the RangeError
        // that a NaN would otherwise produce (`argument-tonumber-throws`).
        let n = self.to_number_coerce(v)?;
        if !n.is_finite() || n.abs() > 8.64e15 {
            return Err(Thrown("RangeError: date value is not finite".into()));
        }
        let t = n.trunc();
        Ok(if t == 0.0 { 0.0 } else { t })
    }

    /// PartitionDateTimeRangePattern: when the two times fall in the same pattern
    /// slot the range collapses to a single formatting with every part "shared";
    /// otherwise the two sides are joined by the en range separator.
    pub(crate) fn dtf_range_parts(
        &self,
        resolved: u32,
        x: f64,
        y: f64,
        fields: u16,
        absolute: bool,
    ) -> Vec<(String, String, &'static str)> {
        let a = self.dtf_parts(resolved, x, fields, absolute);
        let b = self.dtf_parts(resolved, y, fields, absolute);
        if a == b {
            return a
                .into_iter()
                .map(|(t, v)| (t.to_string(), v, "shared"))
                .collect();
        }
        // PartitionDateTimeRangePattern: CLDR's `intervalFormats` give a pattern
        // per (skeleton, greatest differing field) that names each field twice,
        // so the parts the endpoints SHARE are printed once — "Jan 3 – 5, 2019",
        // and "8/4/2021, 12:30:45 AM – 11:30:45 PM" when only the time moves.
        if let Some(parts) = self.dtf_interval_parts(resolved, x, y, fields, absolute) {
            return parts;
        }
        // `intervalFormatFallback` — format both endpoints whole and join.
        let (pre, post) = cldr_en::INTERVAL_FALLBACK
            .split_once("{0}")
            .unwrap_or(("", cldr_en::INTERVAL_FALLBACK));
        let sep = post
            .split_once("{1}")
            .map(|(s, _)| s)
            .unwrap_or(" \u{2013} ");
        let mut out: Vec<(String, String, &'static str)> = vec![];
        if !pre.is_empty() {
            out.push(("literal".to_string(), pre.to_string(), "shared"));
        }
        out.extend(a.into_iter().map(|(t, v)| (t.to_string(), v, "startRange")));
        out.push(("literal".to_string(), sep.to_string(), "shared"));
        out.extend(b.into_iter().map(|(t, v)| (t.to_string(), v, "endRange")));
        out
    }

    /// The `intervalFormats` rendering of a range, or None when CLDR has no
    /// pattern for this skeleton and difference (the caller then falls back).
    fn dtf_interval_parts(
        &self,
        resolved: u32,
        x: f64,
        y: f64,
        fields: u16,
        absolute: bool,
    ) -> Option<Vec<(String, String, &'static str)>> {
        let (d_items, t_items, glue) = self.dtf_pattern_halves(resolved, fields);
        let (_, hour12) = self.dtf_request(resolved, fields);
        // Which half moved decides the shape: a date difference ranges the whole
        // value, a time-only difference keeps the date as a shared prefix.
        let d_diff = self.dtf_greatest_diff(resolved, &d_items, x, y, absolute);
        let t_diff = self.dtf_greatest_diff(resolved, &t_items, x, y, absolute);
        if d_diff.is_some() && !t_items.is_empty() {
            // Both halves in play: CLDR has no combined interval skeleton, and
            // splicing two independent ranges would print the date twice inside
            // one range. The fallback pattern is the specified answer.
            return None;
        }
        let (half, greatest) = match (d_diff, t_diff) {
            (Some(g), _) => (&d_items, g),
            (None, Some(g)) => (&t_items, g),
            (None, None) => return None,
        };
        let pattern = dtf_pattern::interval_pattern(half, hour12, greatest)?;
        let items = dtf_pattern::parse_pattern(&pattern);
        let (sep, first, last) = dtf_pattern::interval_layout(&items)?;
        // Items outside the two ranged runs are common to both endpoints.
        let mut ranged: Vec<(String, String, &'static str)> = vec![];
        let mut push = |vm: &Self, slice: &[dtf_pattern::Item], at: f64, src: &'static str| {
            ranged.extend(
                vm.dtf_render(resolved, at, absolute, slice)
                    .into_iter()
                    .map(|(t, v)| (t.to_string(), v, src)),
            );
        };
        push(self, &items[..first], x, "shared");
        push(self, &items[first..sep], x, "startRange");
        push(self, &items[sep..sep + 1], x, "shared");
        push(self, &items[sep + 1..last + 1], y, "endRange");
        push(self, &items[last + 1..], y, "shared");
        if d_diff.is_some() || d_items.is_empty() {
            return Some(ranged);
        }
        // Time-only difference with a date half: the date is shared, and so is
        // the glue between them.
        let shared: Vec<(String, String, &'static str)> = self
            .dtf_render(resolved, x, absolute, &d_items)
            .into_iter()
            .map(|(t, v)| (t.to_string(), v, "shared"))
            .collect();
        Some(dtf_pattern::splice_glue_parts(
            cldr_en::DATETIME_GLUE_AT[glue],
            &shared,
            &ranged,
        ))
    }

    /// The most significant field in `items` whose rendering differs between the
    /// two instants — the `intervalFormats` key.
    fn dtf_greatest_diff(
        &self,
        resolved: u32,
        items: &[dtf_pattern::Item],
        x: f64,
        y: f64,
        absolute: bool,
    ) -> Option<char> {
        if items.is_empty() {
            return None;
        }
        let a = self.dtf_render(resolved, x, absolute, items);
        let b = self.dtf_render(resolved, y, absolute, items);
        // `dtf_render` emits exactly one part per item, so the three align.
        let mut best: Option<(u8, char)> = None;
        for (i, item) in items.iter().enumerate() {
            let dtf_pattern::Item::Field(c, _) = item else {
                continue;
            };
            if a.get(i).map(|p| &p.1) == b.get(i).map(|p| &p.1) {
                continue;
            }
            let Some(k) = dtf_pattern::class_of_pub(*c) else {
                continue;
            };
            if best.is_none_or(|(bk, _)| k < bk) {
                best = Some((k, *c));
            }
        }
        best.map(|(_, c)| c)
    }

    /// The resolved components as CLDR pattern letters, plus the hour cycle.
    /// This is the request `dtf_pattern::best_pattern` matches against
    /// `availableFormats`; `mask` restricts it to the fields this argument has
    /// (a Temporal.PlainDate contributes no hour, so no hour is requested).
    fn dtf_request(&self, resolved: u32, mask: u16) -> (dtf_pattern::Request, bool) {
        let slot = |k: &str| -> Option<String> {
            match self.heap.get(resolved) {
                HeapObj::Object(m) => m.pos(k).map(|i| self.display(m.vals[i])),
                _ => None,
            }
        };
        let hc = slot("hourCycle").unwrap_or_else(|| "h12".to_string());
        let hour12 = hc == "h11" || hc == "h12";
        let mut req = dtf_pattern::Request::default();
        let has = |bit: u16| mask & bit != 0;
        // Table 7's widths, spelled as UTS #35 field counts: `MMMM` is a wide
        // month, `MMM` an abbreviated one, `MM` two digits, `M` numeric.
        if has(Self::F_ERA) {
            req.push(
                'G',
                match slot("era").as_deref() {
                    Some("long") => 4,
                    Some("narrow") => 5,
                    _ => 1,
                },
            );
        }
        if has(Self::F_YEAR) {
            req.push(
                'y',
                if slot("year").as_deref() == Some("2-digit") {
                    2
                } else {
                    1
                },
            );
        }
        if has(Self::F_MONTH) {
            req.push(
                'M',
                match slot("month").as_deref() {
                    Some("2-digit") => 2,
                    Some("short") => 3,
                    Some("long") => 4,
                    Some("narrow") => 5,
                    _ => 1,
                },
            );
        }
        if has(Self::F_DAY) {
            req.push(
                'd',
                if slot("day").as_deref() == Some("2-digit") {
                    2
                } else {
                    1
                },
            );
        }
        if has(Self::F_WEEKDAY) {
            req.push(
                'E',
                match slot("weekday").as_deref() {
                    Some("long") => 4,
                    Some("narrow") => 5,
                    _ => 3,
                },
            );
        }
        if has(Self::F_DAYPERIOD) {
            req.push(
                'B',
                match slot("dayPeriod").as_deref() {
                    Some("long") => 4,
                    Some("narrow") => 5,
                    _ => 1,
                },
            );
        }
        if has(Self::F_HOUR) {
            let letter = match hc.as_str() {
                "h11" => 'K',
                "h23" => 'H',
                "h24" => 'k',
                _ => 'h',
            };
            req.push(
                letter,
                if slot("hour").as_deref() == Some("2-digit") {
                    2
                } else {
                    1
                },
            );
        }
        if has(Self::F_MINUTE) {
            req.push(
                'm',
                if slot("minute").as_deref() == Some("2-digit") {
                    2
                } else {
                    1
                },
            );
        }
        if has(Self::F_SECOND) {
            req.push(
                's',
                if slot("second").as_deref() == Some("2-digit") {
                    2
                } else {
                    1
                },
            );
        }
        if has(Self::F_FRAC) {
            let n = slot("fractionalSecondDigits").and_then(|s| s.parse::<usize>().ok());
            req.push('S', n.unwrap_or(3).clamp(1, 3));
        }
        if has(Self::F_ZONE) {
            let (c, n) = match slot("timeZoneName").as_deref() {
                Some("long") => ('z', 4),
                Some("shortOffset") => ('O', 1),
                Some("longOffset") => ('O', 4),
                Some("shortGeneric") => ('v', 1),
                Some("longGeneric") => ('v', 4),
                // `timeStyle: "full"` carries `zzzz`, `"long"` carries `z`; with
                // no timeZoneName option at all those are the only two shapes.
                _ if slot("timeZoneName").is_none()
                    && slot("timeStyle").as_deref() == Some("full") =>
                {
                    ('z', 4)
                }
                _ => ('z', 1),
            };
            req.push(c, n);
        }
        (req, hour12)
    }

    /// Keep only the pattern items whose field this argument actually has, and
    /// drop the separators the removed fields owned.
    ///
    /// ECMA-402's Temporal integration builds a SEPARATE pattern per Temporal
    /// type by intersecting the resolved components with that type's fields, so
    /// a `dateStyle: "full"` PlainMonthDay must print "March 4" — the CLDR full
    /// date pattern `EEEE, MMMM d, y` with the weekday, the year, and their
    /// commas gone (`temporal-plainmonthday-formatting-datetime-style.js`).
    /// A removed field takes the literal AFTER it, or, when nothing follows,
    /// the literal before it — except the DAY PERIOD, which takes the literal
    /// BEFORE it. CLDR writes `en`'s medium time pattern as `h:mm:ss\u{202f}a`:
    /// the NARROW NO-BREAK SPACE is the separator that binds "PM" to the clock
    /// time, so under `-u-hc-h23` (which drops `a`) it must go with it and the
    /// ordinary space that followed is what survives — `h:mm:ss\u{202f}a zzzz`
    /// becomes `14:12:47 Coordinated Universal Time`, not
    /// `14:12:47\u{202f}Coordinated Universal Time`
    /// (`format/timedatestyle-en.js`, which hard-codes the plain space).
    fn dtf_filter(
        items: Vec<dtf_pattern::Item>,
        keep: &dyn Fn(char) -> bool,
    ) -> Vec<dtf_pattern::Item> {
        let n = items.len();
        let mut drop = vec![false; n];
        for (i, it) in items.iter().enumerate() {
            let dtf_pattern::Item::Field(c, _) = it else {
                continue;
            };
            if keep(*c) {
                continue;
            }
            drop[i] = true;
            if matches!(c, 'a' | 'b' | 'B')
                && i > 0
                && matches!(items[i - 1], dtf_pattern::Item::Lit(_))
                && !drop[i - 1]
            {
                drop[i - 1] = true;
                continue;
            }
            let later = items[i + 1..]
                .iter()
                .any(|x| matches!(x, dtf_pattern::Item::Field(c, _) if keep(*c)));
            if later {
                if let Some(j) =
                    (i + 1..n).find(|j| matches!(items[*j], dtf_pattern::Item::Lit(_)) && !drop[*j])
                {
                    if j == i + 1 {
                        drop[j] = true;
                    }
                }
            } else if i > 0 && matches!(items[i - 1], dtf_pattern::Item::Lit(_)) {
                drop[i - 1] = true;
            }
        }
        items
            .into_iter()
            .zip(drop)
            .filter(|(_, d)| !d)
            .map(|(it, _)| it)
            .collect()
    }

    /// FormatDateTimePattern (ECMA-402 §11.5.6) over the CLDR `en` patterns in
    /// `cldr_en`, as a typed part list. `format` is this joined; `formatToParts`
    /// is this wrapped.
    ///
    /// The pattern is chosen first — from `dateStyle`/`timeStyle` when they are
    /// present, otherwise by matching the requested components against CLDR's
    /// `availableFormats` — and then interpreted field by field. Before this
    /// engine had the `en` tables it emitted a fixed `M/D/Y, HH:MM:SS`, which no
    /// month name, weekday name, era or `dateStyle` could ever come out of.
    pub(crate) fn dtf_parts(
        &self,
        resolved: u32,
        ms: f64,
        fields: u16,
        absolute: bool,
    ) -> Vec<(&'static str, String)> {
        let (d, t, glue) = self.dtf_pattern_halves(resolved, fields);
        let items = match (d.is_empty(), t.is_empty()) {
            (false, false) => dtf_pattern::splice_glue(cldr_en::DATETIME_GLUE_AT[glue], &d, &t),
            (false, true) => d,
            _ => t,
        };
        self.dtf_render(resolved, ms, absolute, &items)
    }

    /// The pattern for `fields`, split into literals and fields — and kept as
    /// two HALVES, because `formatRange` needs them apart: when only the time
    /// differs, CLDR prints the date once and ranges the time inside it
    /// ("8/4/2021, 12:30:45 AM – 11:30:45 PM").
    fn dtf_pattern_halves(
        &self,
        resolved: u32,
        fields: u16,
    ) -> (Vec<dtf_pattern::Item>, Vec<dtf_pattern::Item>, usize) {
        let slot = |k: &str| -> Option<String> {
            match self.heap.get(resolved) {
                HeapObj::Object(m) => m.pos(k).map(|i| self.display(m.vals[i])),
                _ => None,
            }
        };
        let (req, hour12) = self.dtf_request(resolved, fields);
        // Which CLASSES survive: a field the pattern carries but this argument
        // does not have is dropped by `dtf_filter`.
        let has = |bit: u16| fields & bit != 0;
        let keep = move |c: char| match c {
            'G' => has(Self::F_ERA),
            'y' | 'Y' | 'u' => has(Self::F_YEAR),
            'M' | 'L' => has(Self::F_MONTH),
            'd' => has(Self::F_DAY),
            'E' | 'e' | 'c' => has(Self::F_WEEKDAY),
            // The day period rides with the hour: `h:mm a` keeps its "a" as long
            // as the hour is printed on a 12-hour cycle, and `{minute, second}`
            // never gains one. On h23/h24 it goes, with its separator.
            'a' | 'b' | 'B' => (hour12 && has(Self::F_HOUR)) || has(Self::F_DAYPERIOD),
            'h' | 'H' | 'K' | 'k' => has(Self::F_HOUR),
            'm' => has(Self::F_MINUTE),
            's' => has(Self::F_SECOND),
            'S' => has(Self::F_FRAC),
            'z' | 'Z' | 'O' | 'v' | 'V' | 'X' | 'x' => has(Self::F_ZONE),
            _ => true,
        };
        // dateStyle/timeStyle name CLDR's four stored patterns directly; there
        // is no skeleton matching for them (ECMA-402 DateTimeStylePattern).
        let ds = slot("dateStyle").and_then(|s| dtf_pattern::style_index(&s));
        let ts = slot("timeStyle").and_then(|s| dtf_pattern::style_index(&s));
        if ds.is_some() || ts.is_some() {
            // Each calendar stores its OWN four dateStyle patterns: most want an
            // era that gregorian's do not, hebrew is day-first ("27 Nisan 5760",
            // not "Nisan 27, 5760"), and chinese/dangi use `r(U)`. Falling back
            // to gregorian's produced correct field VALUES in the wrong shape.
            let cal_for_pat = slot("calendar").unwrap_or_else(|| "gregory".to_string());
            let dpat = ds.map(|i| {
                cldr_en::CAL_DATE_FORMATS
                    .iter()
                    .find(|(id, _)| *id == cal_for_pat)
                    .map(|(_, pats)| pats[i])
                    .unwrap_or(cldr_en::DATE_FORMATS[i])
                    .to_string()
            });
            let tpat = ts.map(|i| cldr_en::TIME_FORMATS[i].to_string());
            // A dateStyle pattern is used AS STORED (ECMA-402
            // DateTimeStylePattern) — the style implies its own components, so
            // the era that every non-gregorian calendar's pattern carries must
            // survive the component keep-set, which only knows about explicitly
            // requested fields. The rest of the keep-set still applies, so a
            // Temporal argument that genuinely lacks a field still drops it.
            let d_items = dpat.map(|p| {
                Self::dtf_filter(dtf_pattern::parse_pattern(&p), &|c: char| {
                    c == 'G' || keep(c)
                })
            });
            let t_items = tpat.map(|p| {
                // `en`'s four stored time patterns are 12-hour (`h:mm:ss a …`).
                // An h23/h24 request rewrites the hour field to the padded
                // 24-hour form CLDR uses for it; the day period is then dropped
                // by `keep` above, taking its separator with it.
                let items = dtf_pattern::parse_pattern(&p)
                    .into_iter()
                    .map(|it| match it {
                        dtf_pattern::Item::Field('h', _) if !hour12 => {
                            let hc = slot("hourCycle").unwrap_or_default();
                            dtf_pattern::Item::Field(if hc == "h24" { 'k' } else { 'H' }, 2)
                        }
                        other => other,
                    })
                    .collect();
                Self::dtf_filter(items, &keep)
            });
            // CLDR 42+ keeps a second "at time" glue for exactly this
            // combination; ICU uses it, so `dateStyle: "long"` +
            // `timeStyle: "short"` reads "May 1, 1886 at 2:12 PM".
            return (
                d_items.unwrap_or_default(),
                t_items.unwrap_or_default(),
                ds.unwrap_or(3),
            );
        }
        let (dpat, tpat, glue) = dtf_pattern::best_pattern_halves(&req, hour12);
        (
            Self::dtf_filter(dtf_pattern::parse_pattern(&dpat), &keep),
            Self::dtf_filter(dtf_pattern::parse_pattern(&tpat), &keep),
            glue,
        )
    }

    /// Interpret a parsed pattern against one instant.
    fn dtf_render(
        &self,
        resolved: u32,
        ms: f64,
        absolute: bool,
        items: &[dtf_pattern::Item],
    ) -> Vec<(&'static str, String)> {
        let slot = |k: &str| -> Option<String> {
            match self.heap.get(resolved) {
                HeapObj::Object(m) => m.pos(k).map(|i| self.display(m.vals[i])),
                _ => None,
            }
        };
        // The formatter's zone shifts the instant to a local wall clock. For a
        // named IANA zone that shift depends on the instant itself, so it is
        // looked up at `ms` rather than being a per-zone constant.
        //
        // A PLAIN Temporal value is not an instant: the spec converts its wall
        // clock to an epoch through the formatter's zone and straight back, so
        // the offset cancels and its own fields print unchanged
        // (`temporal-objects-resolved-time-zone.js`).
        let tz_minutes = slot("timeZone")
            .as_deref()
            .and_then(|tz| time_zone_offset_minutes_at(tz, ms as i128))
            .unwrap_or(0);
        let offset_ms = if absolute {
            tz_minutes as i128 * 60_000
        } else {
            0
        };
        let total_ms = ms as i128 + offset_ms;
        let days = total_ms.div_euclid(86_400_000) as i64;
        let (iso_y, iso_mo, iso_d) = epoch_days_to_iso(days);
        let rem_ns = total_ms.rem_euclid(86_400_000) * 1_000_000;
        let t = ns_to_time(rem_ns); // [h, mi, s, ms, us, ns]
                                    // 1970-01-01 was a Thursday, index 4 in CLDR's Sunday-first week.
        let weekday = (days.rem_euclid(7) + 4) as usize % 7;
        // ── the resolved calendar ────────────────────────────────────────────
        // `gregory` and `iso8601` ARE the proleptic Gregorian fields computed
        // above. Any other calendar re-derives (year, month, day) from the same
        // epoch day through vm/temporal's calendar arithmetic — the identical
        // code Temporal uses, so `Intl` and `Temporal` cannot drift apart — and
        // takes its month/era NAMES from the per-calendar CLDR tables.
        let cal_id = slot("calendar").unwrap_or_else(|| "gregory".to_string());
        let cal = match cal_id.as_str() {
            "gregory" | "iso8601" => None,
            other => crate::vm::temporal::calendar::calendar_by_id(other),
        };
        let (y, mo, d) = match cal {
            None => (iso_y, iso_mo, iso_d),
            Some(c) => crate::vm::temporal::calendar::cal_from_epoch_days(c, days),
        };
        // Era ordinal + era year. Proleptic Gregorian has no year 0: CLDR year
        // 1 BC is ISO year 0, so the ERA YEAR is 1 - y below the epoch
        // (`proleptic-gregorian-calendar.js`). A non-gregorian calendar asks its
        // own `cal_era`, which returns the era CODE and the year within it; the
        // code's position in the calendar's era list is the name index.
        let (era_idx, era_year) = match cal {
            None => {
                if y <= 0 {
                    (0usize, 1 - y)
                } else {
                    (1usize, y)
                }
            }
            Some(c) => match crate::vm::temporal::calendar::cal_era(c, y, mo, d) {
                Some((code, ey)) => (cal_era_index(&cal_id, code), ey),
                // A calendar with no eras at all (chinese, dangi): nothing to
                // index, and the `G` field never appears in its patterns.
                None => (0usize, y),
            },
        };
        let minutes_of_day = (t[0] * 60 + t[1]) as i32;
        let mut out: Vec<(&'static str, String)> = vec![];
        for item in items {
            match item {
                dtf_pattern::Item::Lit(s) => out.push(("literal", s.clone())),
                dtf_pattern::Item::Field(c, n) => {
                    let n = *n;
                    // CLDR width index: 0 = wide, 1 = abbreviated, 2 = narrow.
                    let text_width = |n: usize| match n {
                        4 => 0,
                        5 => 2,
                        _ => 1,
                    };
                    match c {
                        'G' => out.push(("era", cal_era_name(&cal_id, era_idx, text_width(n)))),
                        'y' | 'Y' | 'u' => {
                            let v = if *c == 'u' { y } else { era_year };
                            // `yy` is the last two digits, zero-padded; any other
                            // count is a minimum width.
                            let s = if n == 2 {
                                format!("{:02}", v.rem_euclid(100))
                            } else {
                                format!("{:0width$}", v, width = n)
                            };
                            out.push(("year", s));
                        }
                        'M' | 'L' => {
                            let s = match n {
                                1 => mo.to_string(),
                                2 => format!("{mo:02}"),
                                3 => cal_month_name(&cal_id, cal, y, mo, 1),
                                5 => cal_month_name(&cal_id, cal, y, mo, 2),
                                _ => cal_month_name(&cal_id, cal, y, mo, 0),
                            };
                            out.push(("month", s));
                        }
                        'd' => out.push(("day", format!("{:0width$}", d, width = n))),
                        'E' | 'e' | 'c' => {
                            let s = match n {
                                4 => cldr_en::DAYS_WIDE[weekday],
                                5 => cldr_en::DAYS_NARROW[weekday],
                                6 => cldr_en::DAYS_SHORT[weekday],
                                _ => cldr_en::DAYS_ABBR[weekday],
                            };
                            out.push(("weekday", s.to_string()));
                        }
                        'a' | 'b' => {
                            let key = if t[0] < 12 { "am" } else { "pm" };
                            out.push((
                                "dayPeriod",
                                dtf_pattern::day_period_name(key, text_width(n)).to_string(),
                            ));
                        }
                        'B' => {
                            let key = dtf_pattern::day_period_key(minutes_of_day);
                            out.push((
                                "dayPeriod",
                                dtf_pattern::day_period_name(key, text_width(n)).to_string(),
                            ));
                        }
                        'h' | 'H' | 'K' | 'k' => {
                            let h24 = t[0];
                            let v = match c {
                                'h' => {
                                    if h24 % 12 == 0 {
                                        12
                                    } else {
                                        h24 % 12
                                    }
                                }
                                'K' => h24 % 12,
                                'k' => {
                                    if h24 == 0 {
                                        24
                                    } else {
                                        h24
                                    }
                                }
                                _ => h24,
                            };
                            out.push(("hour", format!("{:0width$}", v, width = n)));
                        }
                        'm' => out.push(("minute", format!("{:0width$}", t[1], width = n))),
                        's' => out.push(("second", format!("{:0width$}", t[2], width = n))),
                        'S' => {
                            // fractionalSecond is truncated, never rounded.
                            let ms_str = format!("{:03}", t[3]);
                            let mut s: String = ms_str.chars().take(n).collect();
                            while s.len() < n {
                                s.push('0');
                            }
                            out.push(("fractionalSecond", s));
                        }
                        'z' | 'Z' | 'O' | 'v' | 'V' | 'X' | 'x' => {
                            out.push((
                                "timeZoneName",
                                self.dtf_zone_name(resolved, *c, n, tz_minutes),
                            ));
                        }
                        // A field this engine does not implement (quarter, week
                        // of year, …) is unreachable from ECMA-402's Table 7.
                        _ => {}
                    }
                }
            }
        }
        // The date-time NUMBERS follow the resolved numbering system too
        // (`format/numbering-system.js`); the literals between them do not.
        let ns = slot("numberingSystem").unwrap_or_else(|| "latn".to_string());
        if ns != "latn" {
            for (ty, v) in out.iter_mut() {
                if *ty != "literal" && *ty != "timeZoneName" && *ty != "dayPeriod" {
                    *v = translate_digits(v, &ns);
                }
            }
            // FormatDateTimePattern step 11 formats the fractional seconds with
            // a NumberFormat carrying [[NumberingSystem]], so the separator in
            // front of them is that system's decimal separator, not the
            // pattern's ASCII "." — `en-US-u-nu-arab` prints ٠٦٫٧٨٩.
            if let Some((dec, _)) = numbering_separators(&ns) {
                for i in 1..out.len() {
                    if out[i].0 == "fractionalSecond" && out[i - 1] == ("literal", ".".to_string())
                    {
                        out[i - 1].1 = dec.to_string();
                    }
                }
            }
        }
        out
    }

    /// The zone name a `z`/`v`/`O` field prints.
    ///
    /// There is no CLDR zone-name data here, and UTS #35 §4.5 makes the
    /// *localized GMT format* the specified fallback for exactly that case, so
    /// the name is rendered from the offset this formatter actually used —
    /// self-consistent with the rest of the pattern rather than an invented name.
    ///
    /// The ONE zone that does get its CLDR name is "UTC": `en` gives Etc/UTC the
    /// short name "UTC" and the long name "Coordinated Universal Time", and that
    /// is not interchangeable with the GMT fallback — an OFFSET zone of `+00:00`
    /// still prints "GMT" (`ZonedDateTime/…/toLocaleString/offset-time-zones.js`
    /// asserts the GMT spelling, `…/default-includes-time-and-time-zone-name.js`
    /// the UTC one). The *Offset and *Generic styles stay on the GMT format for
    /// UTC too.
    fn dtf_zone_name(&self, resolved: u32, c: char, n: usize, tz_minutes: i64) -> String {
        let slot = |k: &str| -> Option<String> {
            match self.heap.get(resolved) {
                HeapObj::Object(m) => m.pos(k).map(|i| self.display(m.vals[i])),
                _ => None,
            }
        };
        let m = tz_minutes;
        let utc_named = slot("timeZone").as_deref() == Some("UTC") && c == 'z';
        if utc_named {
            return if n >= 4 {
                "Coordinated Universal Time"
            } else {
                "UTC"
            }
            .to_string();
        }
        if m == 0 {
            return "GMT".to_string();
        }
        let sign = if m < 0 { '-' } else { '+' };
        let (h, mi) = (m.abs() / 60, m.abs() % 60);
        if mi != 0 {
            format!("GMT{sign}{h}:{mi:02}")
        } else {
            format!("GMT{sign}{h}")
        }
    }

    /// Intl.DateTimeFormat.prototype.format(date) — UTC, en-US conventions.
    pub(crate) fn dtf_format(&self, resolved: u32, ms: f64, fields: u16, absolute: bool) -> String {
        self.dtf_parts(resolved, ms, fields, absolute)
            .into_iter()
            .map(|(_, v)| v)
            .collect()
    }

    /// `Temporal.<Type>.prototype.toLocaleString(locales, options)` for every
    /// Temporal type (ECMA-402's Temporal integration). Each type builds an
    /// `Intl.DateTimeFormat` with ITS OWN required/defaults pair and then runs
    /// FormatDateTime over the receiver — which is why `pd.toLocaleString("en")`
    /// prints a date, `pt.toLocaleString("en")` a time, and `zdt.toLocaleString`
    /// both plus a zone name. Temporal.Duration goes to Intl.DurationFormat
    /// instead (it is not a date-time at all).
    pub(crate) fn temporal_to_locale_string(
        &mut self,
        this: Value,
        kind: u8,
        locales: Value,
        options: Value,
    ) -> Result<Value, Thrown> {
        if kind == 0 {
            // sec-temporal.duration.prototype.tolocalestring: construct an
            // Intl.DurationFormat (so the options are validated exactly as the
            // constructor would) and format the receiver with it.
            let df = self.make_intl(native::INTL_DURATIONFORMAT, locales, options)?;
            let resolved = self.intl_this(df, native::INTL_DURATIONFORMAT, "toLocaleString")?;
            let dur = self.to_duration_f64(this)?;
            let parts = self.duration_format_parts(resolved, &dur)?;
            let s: String = parts.into_iter().map(|(_, v, _)| v).collect();
            return Ok(self.alloc_str(s));
        }
        let mode = match kind {
            1 | 5 | 6 => DtfDefaults::Date, // PlainDate / PlainYearMonth / PlainMonthDay
            2 => DtfDefaults::Time,         // PlainTime
            7 => DtfDefaults::Zoned,        // ZonedDateTime
            _ => DtfDefaults::All,          // PlainDateTime / Instant
        };
        // ZonedDateTime formats in ITS OWN zone, so a `timeZone` option would be
        // a second, conflicting answer to the same question: the spec rejects it
        // outright, even when it agrees (`toLocaleString/options-timeZone.js`).
        let zdt_zone = if kind == 7 {
            if options != Value::UNDEFINED {
                self.require_object_coercible(options)?;
                let o = self.to_object(options)?;
                if self.get_prop(o, "timeZone")? != Value::UNDEFINED {
                    return Err(Thrown(
                        "TypeError: ZonedDateTime.toLocaleString does not accept a timeZone option"
                            .into(),
                    ));
                }
            }
            Some(
                self.zdt_tz_id(this.heap_index())
                    .unwrap_or_else(|| "UTC".to_string()),
            )
        } else {
            None
        };
        let dtf = self.make_intl_dtf(native::INTL_DATETIMEFORMAT, locales, options, mode)?;
        let resolved = self.intl_this(dtf, native::INTL_DATETIMEFORMAT, "toLocaleString")?;
        if let Some(tz) = zdt_zone {
            // Step "CreateDataPropertyOrThrow(optionsCopy, "timeZone",
            // zonedDateTime.[[TimeZone]])" — done on the resolved record, since
            // the constructor above already validated everything else.
            let v = self.alloc_str(tz);
            if let HeapObj::Object(m) = self.heap.get_mut(resolved) {
                m.set("timeZone", v);
            }
        }
        self.dtf_check_calendar(resolved, this, "toLocaleString")?;
        let (fields, absolute) = self.dtf_fields_for_kind(resolved, kind, "toLocaleString")?;
        let ms = if kind == 7 {
            (self.zdt_epoch_ns(this.heap_index()).unwrap_or(0) / 1_000_000) as f64
        } else {
            self.dtf_time_value(this)?
        };
        let s = self.dtf_format(resolved, ms, fields, absolute);
        Ok(self.alloc_str(s))
    }
}

// ── per-calendar CLDR name lookup ───────────────────────────────────────────
// `gregory`/`iso8601` keep the top-level MONTHS_*/ERAS_* tables; every other
// calendar reads `cldr_en::CAL_MONTHS` / `CAL_ERAS`, both generated from the
// same CLDR release by `tools/gen_cldr_en.py`.

/// The name index for era CODE `code` in calendar `cal_id`. `cal_era` returns
/// the spec's era code (`"be"`, `"ah"`, `"ce"`, …); CLDR stores era names in
/// ordinal order, and for every calendar zipp implements the ordinal is simply
/// the position of that code in the calendar's own era list — 0 for the single
/// -era calendars, and 0/1 (reverse/forward) for the two-era ones.
fn cal_era_index(cal_id: &str, code: &str) -> usize {
    // Japanese is the one calendar with a long era list: CLDR carries all 237
    // historical nengo, and the five modern ones sit at the end. `cal_era`
    // only ever produces those five plus the ce/bce fallback for pre-Meiji
    // dates, which CLDR indexes at 0/1 like any two-era calendar.
    if cal_id == "japanese" {
        return match code {
            "meiji" => 232,
            "taisho" => 233,
            "showa" => 234,
            "heisei" => 235,
            "reiwa" => 236,
            "bce" | "bc" => 0,
            _ => 1,
        };
    }
    match (cal_id, code) {
        // Two-era calendars: the BEFORE era is ordinal 0.
        (_, "bce") | (_, "bc") | (_, "broc") => 0,
        // Coptic and Ethiopic both store a pre-era at 0 and the era actually in
        // use at 1 (CLDR spells the coptic pair "ERA0"/"ERA1"); `cal_era`'s
        // "am" is the latter for both. Ethioaa has a single era.
        ("roc", "roc") | ("ethiopic", "am") | ("coptic", "am") => 1,
        (_, "ce") | (_, "ad") => 1,
        // Single-era calendars (buddhist BE, islamic AH, hebrew AM, persian AP,
        // indian Saka, ethioaa) have exactly one name.
        _ => 0,
    }
}

/// The era NAME for `cal_id` at ordinal `idx` and CLDR width (0 wide, 1 abbr,
/// 2 narrow). Falls back to the gregorian table for gregory/iso8601, and to the
/// empty string when a calendar carries no era names at that width (chinese and
/// dangi have none at all — their patterns never contain `G`).
fn cal_era_name(cal_id: &str, idx: usize, width: usize) -> String {
    if cal_id == "gregory" || cal_id == "iso8601" {
        let t = match width {
            0 => &cldr_en::ERAS_WIDE[..],
            2 => &cldr_en::ERAS_NARROW[..],
            _ => &cldr_en::ERAS_ABBR[..],
        };
        return t.get(idx).copied().unwrap_or_default().to_string();
    }
    for (id, wide, abbr, narrow) in cldr_en::CAL_ERAS {
        if *id == cal_id {
            let t = match width {
                0 => wide,
                2 => narrow,
                _ => abbr,
            };
            // A width CLDR does not carry for this calendar falls back to the
            // abbreviated list, which every era-bearing calendar has.
            return t
                .get(idx)
                .or_else(|| abbr.get(idx))
                .copied()
                .unwrap_or_default()
                .to_string();
        }
    }
    String::new()
}

/// The month NAME for `cal_id`, month `mo` of calendar year `y`, at CLDR width
/// (0 wide, 1 abbr, 2 narrow).
///
/// Hebrew is the one calendar whose month NAMES depend on the year: in a leap
/// year an extra month is inserted, so CLDR carries a 13th name plus a separate
/// "Adar II". vm/temporal already renumbers the months, so month 7 of a leap
/// year is the one that renames — and its name is the extra trailing entry the
/// generator appends.
fn cal_month_name(
    cal_id: &str,
    cal: Option<crate::vm::temporal::calendar::Cal>,
    y: i64,
    mo: i64,
    width: usize,
) -> String {
    if cal_id == "gregory" || cal_id == "iso8601" {
        let i = (mo - 1) as usize;
        let t = match width {
            0 => &cldr_en::MONTHS_WIDE[..],
            2 => &cldr_en::MONTHS_NARROW[..],
            _ => &cldr_en::MONTHS_ABBR[..],
        };
        return t.get(i).copied().unwrap_or_default().to_string();
    }
    for (id, wide, abbr, narrow) in cldr_en::CAL_MONTHS {
        if *id != cal_id {
            continue;
        }
        let t = match width {
            0 => wide,
            2 => narrow,
            _ => abbr,
        };
        let mut i = (mo - 1) as usize;
        if cal_id == "hebrew" {
            let leap = cal
                .map(|c| crate::vm::temporal::calendar::cal_in_leap_year(c, y))
                .unwrap_or(false);
            // The trailing entry is Adar II; it is month 7 only in a leap year.
            if leap && mo == 7 {
                i = t.len() - 1;
            }
        }
        return t.get(i).copied().unwrap_or_default().to_string();
    }
    String::new()
}
