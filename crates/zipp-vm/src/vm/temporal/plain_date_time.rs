// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    pub(crate) fn make_plain_date_time(&mut self, f: [i64; 9]) -> Result<Value, Thrown> {
        if !(1..=12).contains(&f[1])
            || f[2] < 1
            || f[2] > days_in_month(f[0], f[1])
            || !(-271821..=275760).contains(&f[0])
            || !(0..24).contains(&f[3])
            || !(0..60).contains(&f[4])
            || !(0..60).contains(&f[5])
            || !(0..1000).contains(&f[6])
            || !(0..1000).contains(&f[7])
            || !(0..1000).contains(&f[8])
            || !iso_datetime_ns_in_range(f)
        {
            return Err(Thrown("RangeError: invalid PlainDateTime value".into()));
        }
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 3, fields: f.to_vec() });
        if self.plaindatetime_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.plaindatetime_proto));
        }
        Ok(Value::heap(idx))
    }

    pub(crate) fn pdt_fields(&self, idx: u32) -> Option<[i64; 9]> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 3, fields } => {
                let mut f = [0i64; 9];
                for (i, s) in f.iter_mut().enumerate() {
                    *s = *fields.get(i).unwrap_or(&0);
                }
                Some(f)
            }
            _ => None,
        }
    }

    pub(crate) fn to_plain_date_time(&mut self, v: Value) -> Result<[i64; 9], Thrown> {
        self.to_plain_date_time_overflow(v, false)
    }

    /// Like `to_plain_date_time` but enforces ISODateTimeWithinLimits on the
    /// result — required by compare/equals/since/until, whose argument must be a
    /// representable PlainDateTime. The constructor/from() bound this via
    /// `make_plain_date_time`; these methods reach the parsed fields directly.
    /// NOT used by the `relativeTo` path, where a bare date string is a
    /// day-granular PlainDate (so the minimum -271821-04-19 midnight stays valid).
    pub(crate) fn to_plain_date_time_limited(&mut self, v: Value) -> Result<[i64; 9], Thrown> {
        let f = self.to_plain_date_time(v)?;
        if !iso_datetime_ns_in_range(f) {
            return Err(Thrown(
                "RangeError: date-time is outside the representable range".into(),
            ));
        }
        Ok(f)
    }

    pub(crate) fn to_plain_date_time_overflow(
        &mut self,
        v: Value,
        reject: bool,
    ) -> Result<[i64; 9], Thrown> {
        if v.is_heap() {
            if let Some(f) = self.pdt_fields(v.heap_index()) {
                return Ok(f);
            }
            if let Some((y, m, d)) = self.plain_date_fields(v.heap_index()) {
                return Ok([y, m, d, 0, 0, 0, 0, 0, 0]);
            }
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                if !temporal_string_ok(&s, true, true) {
                    return Err(Thrown(format!("RangeError: invalid datetime string '{s}'")));
                }
                return parse_iso_datetime(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid datetime string '{s}'")));
            }
            if self.is_object_value(v) {
                let bag = self.read_pdt_bag(v, false)?;
                return Self::finish_pdt_fields(&bag, reject);
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.PlainDateTime".into()))
    }

    /// PrepareTemporalFields/PrepareCalendarFields for a PlainDateTime-like
    /// property bag: validates the calendar, then reads EVERY field via
    /// observable Gets in ALPHABETICAL order — day, hour, microsecond,
    /// millisecond, minute, month+monthCode, nanosecond, [offset,] second,
    /// [timeZone,] year — with the offset/timeZone reads (ZonedDateTime-like
    /// bags, `with_offset_tz`) at their alphabetical slots. The required y/m/d
    /// presence TypeError is raised here, at the end of field preparation;
    /// the deferred monthCode-validity RangeError and the overflow regulation
    /// live in [`Self::finish_pdt_fields`] so callers can read their options
    /// bag in between (order-of-operations).
    /// Slots: 0=year 1=month 2=day 3=hour 4=minute 5=second 6=ms 7=us 8=ns.
    pub(crate) fn read_pdt_bag(&mut self, v: Value, with_offset_tz: bool) -> Result<PdtBag, Thrown> {
        self.validate_iso_calendar_field(v)?;
        let mut f = [0i64; 9];
        let mut have_date = [false; 3];
        let mut month_code_invalid = false;
        if let Some(x) = self.opt_int_field(v, "day")? {
            f[2] = x;
            have_date[2] = true;
        }
        if let Some(x) = self.opt_int_field(v, "hour")? {
            f[3] = x;
        }
        if let Some(x) = self.opt_int_field(v, "microsecond")? {
            f[7] = x;
        }
        if let Some(x) = self.opt_int_field(v, "millisecond")? {
            f[6] = x;
        }
        if let Some(x) = self.opt_int_field(v, "minute")? {
            f[4] = x;
        }
        if let Some((x, valid)) = self.read_month_field_raw(v)? {
            // month then monthCode (alphabetical, inside read_month_field_raw);
            // a calendar-invalid monthCode's RangeError is deferred to
            // finish_pdt_fields.
            f[1] = x;
            have_date[1] = true;
            month_code_invalid = !valid;
        }
        if let Some(x) = self.opt_int_field(v, "nanosecond")? {
            f[8] = x;
        }
        let bag_off = if with_offset_tz { self.validate_bag_offset_field(v)? } else { None };
        if let Some(x) = self.opt_int_field(v, "second")? {
            f[5] = x;
        }
        let tz = if with_offset_tz { self.get_prop(v, "timeZone")? } else { Value::UNDEFINED };
        if let Some(x) = self.opt_int_field(v, "year")? {
            f[0] = x;
            have_date[0] = true;
        }
        if !have_date.iter().all(|&b| b) {
            return Err(Thrown("TypeError: PlainDateTime-like requires year, month, day".into()));
        }
        Ok(PdtBag { f, month_code_invalid, bag_off, tz })
    }

    /// InterpretTemporalDateTimeFields: the deferred calendar-invalid monthCode
    /// RangeError (after the caller's options reads), then constrain/reject
    /// regulation of the date and time fields.
    pub(crate) fn finish_pdt_fields(bag: &PdtBag, reject: bool) -> Result<[i64; 9], Thrown> {
        let mut f = bag.f;
        if bag.month_code_invalid {
            return Err(Thrown(
                "RangeError: monthCode is not valid for the ISO 8601 calendar".into(),
            ));
        }
        // date: month/day; time: hour..nanosecond (maxes 23/59/59/999/999/999).
        let maxes = [23, 59, 59, 999, 999, 999];
        if reject {
            if !(1..=12).contains(&f[1]) || f[2] < 1 || f[2] > days_in_month(f[0], f[1]) {
                return Err(Thrown("RangeError: invalid date fields".into()));
            }
            for (i, &mx) in maxes.iter().enumerate() {
                if f[3 + i] < 0 || f[3 + i] > mx {
                    return Err(Thrown("RangeError: time field out of range".into()));
                }
            }
        } else {
            // "constrain" clamps only the upper bound; month/day below 1 is a
            // hard floor that rejects (time fields legitimately clamp up from 0).
            if f[1] < 1 || f[2] < 1 {
                return Err(Thrown("RangeError: invalid date fields".into()));
            }
            f[1] = f[1].min(12);
            f[2] = f[2].min(days_in_month(f[0], f[1]));
            for (i, &mx) in maxes.iter().enumerate() {
                f[3 + i] = f[3 + i].clamp(0, mx);
            }
        }
        Ok(f)
    }

    pub(crate) fn plain_date_time_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let f = match self.pdt_fields(idx) {
            Some(f) => f,
            None => return Ok(None),
        };
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let date = [f[0], f[1], f[2]];
        let time = [f[3], f[4], f[5], f[6], f[7], f[8]];
        match name {
            "toJSON" => {
                let s = format!("{}T{}", iso_date_string(date[0], date[1], date[2]), time_string(&time));
                Ok(Some(self.alloc_str(s)))
            }
            "toString" => {
                // Options read alphabetically: calendarName precedes the
                // fractionalSecondDigits/roundingMode/smallestUnit triplet;
                // smallestUnit is last, so time_precision's read-time unit-class
                // RangeError is observably in the right place for PlainDateTime.
                let suf = self.calendar_name_suffix(a0)?;
                let (unit, digits, omit, mode) = self.time_precision(a0)?;
                let rounded = round_increment(time_to_ns(&time), unit, &mode);
                let carry = rounded.div_euclid(DAY_NS) as i64;
                let t = ns_to_time(rounded.rem_euclid(DAY_NS));
                let (ny, nm, nd) =
                    epoch_days_to_iso(iso_to_epoch_days(date[0], date[1], date[2]) + carry);
                // RoundISODateTime: the rounded result must re-satisfy
                // ISODateTimeWithinLimits (a day carry at the boundary can push an
                // in-range datetime exactly onto the exclusive limit).
                if !iso_datetime_ns_in_range([ny, nm, nd, t[0], t[1], t[2], t[3], t[4], t[5]]) {
                    return Err(Thrown(
                        "RangeError: rounded PlainDateTime is outside the representable range"
                            .into(),
                    ));
                }
                let s = format!(
                    "{}T{}{}",
                    iso_date_string(ny, nm, nd),
                    format_time_part(&t, digits, omit),
                    suf
                );
                Ok(Some(self.alloc_str(s)))
            }
            "valueOf" => {
                Err(Thrown("TypeError: Called Temporal.PlainDateTime.prototype.valueOf".into()))
            }
            "equals" => {
                let o = self.to_plain_date_time_limited(a0)?;
                Ok(Some(Value::bool(f == o)))
            }
            "toPlainDate" => Ok(Some(self.make_plain_date(date[0], date[1], date[2])?)),
            "toPlainTime" => Ok(Some(self.make_plain_time(time)?)),
            "toPlainYearMonth" => Ok(Some(self.make_plain_year_month(date[0], date[1], 1)?)),
            "toPlainMonthDay" => Ok(Some(self.make_plain_month_day(date[1], date[2], 1972)?)),
            "withCalendar" => {
                // ToTemporalCalendarIdentifier(undefined) is a TypeError — the
                // argument is required (validate_calendar_value allows undefined for
                // an optional field-bag calendar, so guard here, not there).
                if a0 == Value::UNDEFINED {
                    return Err(Thrown("TypeError: withCalendar requires a calendar argument".into()));
                }
                self.validate_calendar_value(a0)?;
                Ok(Some(self.make_plain_date_time(f)?))
            }
            "withPlainDate" => {
                let nd = self.to_plain_date(a0)?;
                Ok(Some(self.make_plain_date_time([
                    nd.0, nd.1, nd.2, f[3], f[4], f[5], f[6], f[7], f[8],
                ])?))
            }
            "withPlainTime" => {
                // Keep the date, replace the time (ToTemporalTime; default midnight).
                let nt = if a0 == Value::UNDEFINED { [0i64; 6] } else { self.to_plain_time(a0)? };
                Ok(Some(self.make_plain_date_time([
                    date[0], date[1], date[2], nt[0], nt[1], nt[2], nt[3], nt[4], nt[5],
                ])?))
            }
            "toZonedDateTime" => {
                // Validate the options bag and the disambiguation enum (this method
                // reads only options.disambiguation per spec) before building.
                let opts = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if opts != Value::UNDEFINED && !self.is_object_value(opts) {
                    return Err(Thrown("TypeError: options must be an object or undefined".into()));
                }
                self.opt_string(
                    opts,
                    "disambiguation",
                    "compatible",
                    &["compatible", "earlier", "later", "reject"],
                )?;
                let (id, offset) = self.parse_tz_arg(a0)?;
                let local = (iso_to_epoch_days(f[0], f[1], f[2]) as i128) * DAY_NS
                    + time_to_ns(&[f[3], f[4], f[5], f[6], f[7], f[8]]);
                Ok(Some(self.alloc_zdt(local - offset as i128, offset, id)?))
            }
            "with" => {
                self.reject_temporal_like(a0)?;
                // Read all fields (observable getters) in alphabetical key order, BEFORE
                // the options bag — each pair is (key, index into the [y,mo,d,h,mi,s,ms,
                // us,ns] field array). The `month` slot goes through read_month_field_raw
                // so monthCode is honoured, month/monthCode agreement is enforced, and a
                // calendar-invalid code is deferred (month_valid=false) until after the
                // options bag is read.
                let order: [(&str, usize); 9] = [
                    ("day", 2),
                    ("hour", 3),
                    ("microsecond", 7),
                    ("millisecond", 6),
                    ("minute", 4),
                    ("month", 1),
                    ("nanosecond", 8),
                    ("second", 5),
                    ("year", 0),
                ];
                let mut raw: [Option<i64>; 9] = [None; 9];
                let mut month_valid = true;
                let mut any = false;
                for (key, slot) in order {
                    let v = if key == "month" {
                        self.read_month_field_raw(a0)?.map(|(mm, valid)| {
                            month_valid = valid;
                            mm
                        })
                    } else {
                        self.opt_int_field(a0, key)?
                    };
                    if let Some(x) = v {
                        raw[slot] = Some(x);
                        any = true;
                    }
                }
                if !any {
                    return Err(Thrown("TypeError: with() requires a partial object".into()));
                }
                let mut nf = f;
                for (i, slot) in raw.iter().enumerate() {
                    if let Some(x) = *slot {
                        nf[i] = x;
                    }
                }
                // month/day use ToPositiveIntegerWithTruncation: a value below 1 is
                // rejected during field preparation, BEFORE the options bag is read.
                if nf[1] < 1 || nf[2] < 1 {
                    return Err(Thrown("RangeError: invalid date fields".into()));
                }
                let reject = self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                // A well-formed-but-calendar-invalid monthCode ("M08L", "M13") is
                // rejected only after the options bag has been read.
                if !month_valid {
                    return Err(Thrown(
                        "RangeError: monthCode is not valid for the ISO 8601 calendar".into(),
                    ));
                }
                if !reject {
                    // "constrain" clamps only the UPPER bound.
                    nf[1] = nf[1].min(12);
                    nf[2] = nf[2].min(days_in_month(nf[0], nf[1]));
                    let maxes = [23, 59, 59, 999, 999, 999];
                    for (i, &mx) in maxes.iter().enumerate() {
                        nf[3 + i] = nf[3 + i].clamp(0, mx);
                    }
                }
                Ok(Some(self.make_plain_date_time(nf)?))
            }
            "add" | "subtract" => {
                let dur = self.to_duration(a0)?;
                let reject = self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                let sign: i64 = if name == "add" { 1 } else { -1 };
                // Time part with day carry.
                let tns = time_to_ns(&time)
                    + ((dur[4] as i128) * 3_600_000_000_000
                        + (dur[5] as i128) * 60_000_000_000
                        + (dur[6] as i128) * 1_000_000_000
                        + (dur[7] as i128) * 1_000_000
                        + (dur[8] as i128) * 1_000
                        + (dur[9] as i128))
                        * sign as i128;
                let carry = tns.div_euclid(86_400_000_000_000) as i64;
                let nt = ns_to_time(tns.rem_euclid(86_400_000_000_000));
                // Date part: years/months constrain (or reject), then weeks/days + carry.
                let tm = (date[0] + dur[0] * sign) * 12 + (date[1] - 1) + dur[1] * sign;
                let ny0 = tm.div_euclid(12);
                let nmo = tm.rem_euclid(12) + 1;
                if reject && date[2] > days_in_month(ny0, nmo) {
                    return Err(Thrown("RangeError: date arithmetic overflows the month".into()));
                }
                let nd0 = date[2].min(days_in_month(ny0, nmo));
                let ed = iso_to_epoch_days(ny0, nmo, nd0) + (dur[2] * 7 + dur[3]) * sign + carry;
                let (ny, nm, nd) = epoch_days_to_iso(ed);
                Ok(Some(self.make_plain_date_time([
                    ny, nm, nd, nt[0], nt[1], nt[2], nt[3], nt[4], nt[5],
                ])?))
            }
            "until" | "since" => {
                let o = self.to_plain_date_time_limited(a0)?;
                let opts = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if opts != Value::UNDEFINED && !self.is_object_value(opts) {
                    return Err(Thrown("TypeError: options must be an object or undefined".into()));
                }
                let all_units = &[
                    "auto", "year", "years", "month", "months", "week", "weeks", "day", "days",
                    "hour", "hours", "minute", "minutes", "second", "seconds", "millisecond",
                    "milliseconds", "microsecond", "microseconds", "nanosecond", "nanoseconds",
                ];
                // GetDifferenceSettings order-of-operations: read+cast all options
                // (largestUnit, roundingIncrement, roundingMode, smallestUnit) before
                // validating. smallestUnit default "nanosecond"; largestUnit default
                // "auto" → the larger of smallestUnit and "day".
                let largest_str = self.opt_string_raw(opts, "largestUnit", "auto")?;
                let inc = self.read_rounding_increment(opts)?;
                let mode = self.read_rounding_mode(opts, "trunc")?;
                let smallest_str = self.opt_string_raw(opts, "smallestUnit", "nanosecond")?;
                self.unit_allowed(&largest_str, "largestUnit", all_units)?;
                self.unit_allowed(&smallest_str, "smallestUnit", all_units)?;
                let smallest = normalize_unit(&smallest_str, "nanosecond");
                let largest_raw = normalize_unit(&largest_str, "auto");
                let order = [
                    "year", "month", "week", "day", "hour", "minute", "second", "millisecond",
                    "microsecond", "nanosecond",
                ];
                let rank = |u: &str| order.iter().position(|&x| x == u).unwrap_or(9);
                let largest = if largest_raw == "auto" {
                    if rank(&smallest) < rank("day") { smallest.clone() } else { "day".to_string() }
                } else {
                    largest_raw
                };
                if rank(&smallest) < rank(&largest) {
                    return Err(Thrown(
                        "RangeError: smallestUnit is larger than largestUnit".into(),
                    ));
                }
                // A time-unit increment must evenly divide its next-highest unit
                // (day/week/month/year carry no per-unit bound here).
                if let Some(max) = max_increment(&smallest) {
                    if inc >= max || max % inc != 0 {
                        return Err(Thrown(
                            "RangeError: roundingIncrement must evenly divide the next unit".into(),
                        ));
                    }
                }
                // since = negate(until): forward (this → other) difference with a
                // sign-negated rounding mode, then negate the result.
                let (dt1, dt2) = (f, o);
                let eff = if name == "since" { negate_mode(&mode) } else { mode.clone() };
                let df = difference_datetime(dt1, dt2, &largest);
                // With no calendar units (largestUnit ≤ day) the difference is an
                // exact nanosecond span: round it and re-balance. Calendar-unit
                // largestUnits round the fractional remainder against the anchor.
                let mut out = if rank(&largest) >= rank("day") {
                    let total_ns = (df[3] as i128) * DAY_NS
                        + time_to_ns(&[df[4], df[5], df[6], df[7], df[8], df[9]]);
                    let inc_ns = unit_ns(&smallest) * inc;
                    let rounded = round_increment(total_ns, inc_ns, &eff);
                    balance_duration_ns(rounded, &largest)?
                } else if matches!(smallest.as_str(), "year" | "month" | "week") {
                    round_relative_datetime_diff(dt1, dt2, &smallest, &largest, inc, &eff)?
                        .map(|x| x as f64)
                } else {
                    round_datetime_diff_daytime(dt1, df, &smallest, &largest, inc, &eff)
                        .map(|x| x as f64)
                };
                if name == "since" {
                    out.iter_mut().for_each(|x| *x = -*x);
                }
                Ok(Some(self.make_duration(out)))
            }
            "round" => {
                let (su, inc, mode) = self.read_round_options(
                    a0,
                    &[
                        "day", "hour", "minute", "second", "millisecond", "microsecond",
                        "nanosecond",
                    ],
                    true,
                )?;
                let time_ns = time_to_ns(&time);
                let inc_ns = unit_ns(&su) * inc;
                let rounded = round_increment(time_ns, inc_ns, &mode);
                let day_carry = rounded.div_euclid(DAY_NS) as i64;
                let nt = ns_to_time(rounded.rem_euclid(DAY_NS));
                let ed = iso_to_epoch_days(date[0], date[1], date[2]) + day_carry;
                let (ny, nm, nd) = epoch_days_to_iso(ed);
                Ok(Some(self.make_plain_date_time([
                    ny, nm, nd, nt[0], nt[1], nt[2], nt[3], nt[4], nt[5],
                ])?))
            }
            "getISOFields" => {
                let cal = self.alloc_str("iso8601".to_string());
                let names = [
                    "isoYear",
                    "isoMonth",
                    "isoDay",
                    "isoHour",
                    "isoMinute",
                    "isoSecond",
                    "isoMillisecond",
                    "isoMicrosecond",
                    "isoNanosecond",
                ];
                let mut o = ObjMap::new();
                for (i, nm) in names.iter().enumerate() {
                    o.set(nm, Value::num(f[i] as f64));
                }
                o.set("calendar", cal);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Object(Box::new(o))))))
            }
            _ => Ok(None),
        }
    }

    // ── Temporal.Instant ──

}
