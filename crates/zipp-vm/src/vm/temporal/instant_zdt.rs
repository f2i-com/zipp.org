// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    /// Current wall-clock time as nanoseconds since the Unix epoch.
    pub(crate) fn now_epoch_ns() -> i128 {
        crate::vm::clock::now_epoch_ns()
    }

    pub(crate) fn make_instant(&mut self, ns: i128) -> Result<Value, Thrown> {
        if ns.abs() > 8_640_000_000_000_000_000_000 {
            return Err(Thrown("RangeError: Instant outside the supported range".into()));
        }
        let hi = (ns >> 64) as i64;
        let lo = ns as i64;
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 4, fields: vec![hi, lo] });
        if self.instant_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.instant_proto));
        }
        Ok(Value::heap(idx))
    }

    pub(crate) fn instant_ns(&self, idx: u32) -> Option<i128> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 4, fields } => {
                Some(((fields[0] as i128) << 64) | ((fields[1] as u64) as i128))
            }
            _ => None,
        }
    }

    /// `new Temporal.ZonedDateTime(epochNanoseconds, timeZone[, calendar])`. The
    /// instant is `fields = [ns hi, ns lo, offsetNanoseconds]` and the time-zone id
    /// is held in `zdt_tz` (GC-traced). Stage 1: UTC + numeric-offset zones carry a
    /// real offset; a named zone is accepted with offset 0 (no tz database yet).
    /// Whether `s` (starting with '+'/'-') is a syntactically valid time-zone
    /// offset IDENTIFIER: `±HH`, `±HHMM`, or `±HH:MM` — minute precision at
    /// most (digit-range validation happens in the actual offset parse).
    pub(crate) fn offset_identifier_shape_ok(s: &str) -> bool {
        let rest = &s.as_bytes()[1..];
        match rest.len() {
            2 | 4 => rest.iter().all(u8::is_ascii_digit),
            5 => {
                rest[2] == b':'
                    && rest[..2].iter().all(u8::is_ascii_digit)
                    && rest[3..].iter().all(u8::is_ascii_digit)
            }
            _ => false,
        }
    }

    pub(crate) fn make_zoned_date_time(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let _gc = self.gc_lock_guard();
        // Beyond-i128 saturates (sign preserved) — certainly out of range below.
        let ns = self.to_bigint(args.first().copied().unwrap_or(Value::UNDEFINED))?.to_i128_sat();
        if ns.abs() > 8_640_000_000_000_000_000_000 {
            return Err(Thrown("RangeError: ZonedDateTime outside the supported range".into()));
        }
        let tzarg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        if tzarg == Value::UNDEFINED {
            return Err(Thrown("TypeError: Temporal.ZonedDateTime requires a time zone".into()));
        }
        // The CONSTRUCTOR's time zone is an IDENTIFIER (ParseTimeZoneIdentifier):
        // a named id or a minute-precision UTC offset. An ISO datetime string
        // with a bracketed annotation and a sub-minute offset are RangeErrors
        // here, though both are valid for from()/property-bag timeZone fields
        // (ToTemporalTimeZoneIdentifier extracts the bracket there).
        if tzarg.is_heap() && self.heap.is_str_like(tzarg.heap_index()) {
            let s = self.heap.str_cow(tzarg.heap_index()).unwrap().into_owned();
            let t = s.trim();
            if t.contains('[')
                || (t.starts_with(['+', '-']) && !Self::offset_identifier_shape_ok(t))
            {
                return Err(Thrown(format!("RangeError: invalid time zone \"{s}\"")));
            }
        }
        // ToTemporalTimeZoneIdentifier: a wrong-type time zone is a TypeError.
        let (id, offset_ns) = self.parse_tz_arg(tzarg)?;
        let hi = (ns >> 64) as i64;
        let lo = ns as i64;
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 7, fields: vec![hi, lo, offset_ns] });
        if self.zoneddatetime_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.zoneddatetime_proto));
        }
        let idv = self.alloc_str(id);
        self.zdt_tz.insert(idx, idv);
        Ok(Value::heap(idx))
    }

    /// The epoch nanoseconds of a ZonedDateTime instance.
    pub(crate) fn zdt_epoch_ns(&self, idx: u32) -> Option<i128> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 7, fields } => {
                Some(((fields[0] as i128) << 64) | ((fields[1] as u64) as i128))
            }
            _ => None,
        }
    }

    /// The UTC offset (nanoseconds) of a ZonedDateTime instance — for a NAMED
    /// zone that is a function of the instant, so it is recomputed from the tz
    /// database rather than read back from the slot the constructor filled in
    /// (which only ever holds an offset zone's fixed value).
    pub(crate) fn zdt_offset_ns(&self, idx: u32) -> i64 {
        let stored = match self.heap.get(idx) {
            HeapObj::Temporal { kind: 7, fields } => fields.get(2).copied().unwrap_or(0),
            _ => return 0,
        };
        match self.zdt_tz_id(idx) {
            Some(id) => tz_offset_ns_at(&id, self.zdt_epoch_ns(idx).unwrap_or(0)),
            None => stored,
        }
    }

    /// Local broadcast wall-clock fields [y,mo,d,h,mi,s,ms,us,ns] for a ZDT.
    pub(crate) fn zdt_local(&self, idx: u32) -> [i64; 9] {
        let epoch = self.zdt_epoch_ns(idx).unwrap_or(0);
        let local = epoch + self.zdt_offset_ns(idx) as i128;
        let days = local.div_euclid(DAY_NS) as i64;
        let (y, mo, d) = epoch_days_to_iso(days);
        let t = ns_to_time(local.rem_euclid(DAY_NS));
        [y, mo, d, t[0], t[1], t[2], t[3], t[4], t[5]]
    }

    /// `Temporal.ZonedDateTime.prototype` methods (stage 1: valueOf throws,
    /// toJSON/toString format; arithmetic not yet implemented).
    pub(crate) fn zoned_date_time_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let cal = self.cal_of(idx);
        match name {
            "valueOf" => Err(Thrown(
                "TypeError: Called Temporal.ZonedDateTime.prototype.valueOf which always throws"
                    .into(),
            )),
            "toString" => {
                let opts = args.first().copied().unwrap_or(Value::UNDEFINED);
                let s = self.zdt_to_string_opts(idx, opts)?;
                Ok(Some(self.alloc_str(s)))
            }
            "toJSON" | "toLocaleString" => {
                let s = self.zdt_to_string(idx);
                Ok(Some(self.alloc_str(s)))
            }
            "toInstant" => {
                let ns = self.zdt_epoch_ns(idx).unwrap_or(0);
                Ok(Some(self.make_instant(ns)?))
            }
            "toPlainDateTime" => {
                let f = self.zdt_local(idx);
                let r = self.make_plain_date_time(f)?;
                Ok(Some(self.tag_cal(r, cal)))
            }
            "toPlainDate" => {
                let f = self.zdt_local(idx);
                let r = self.make_plain_date(f[0], f[1], f[2])?;
                Ok(Some(self.tag_cal(r, cal)))
            }
            "toPlainTime" => {
                let f = self.zdt_local(idx);
                Ok(Some(self.make_plain_time([f[3], f[4], f[5], f[6], f[7], f[8]])?))
            }
            "startOfDay" => {
                // GetStartOfDay: local midnight resolved through the zone with
                // "compatible" disambiguation — in a zone that springs forward
                // AT midnight (America/Sao_Paulo used to) the day starts at
                // 01:00, not at an instant that never happened.
                let off = self.zdt_offset_ns(idx) as i128;
                let local = self.zdt_epoch_ns(idx).unwrap_or(0) + off;
                let midnight_local = local.div_euclid(DAY_NS) * DAY_NS;
                let id = self.zdt_tz_id(idx).unwrap_or_else(|| "UTC".to_string());
                let new_ns = tz_start_of_day(&id, midnight_local)?;
                Ok(Some(self.make_zoned_date_time_raw(new_ns, self.zdt_offset_ns(idx), idx)?))
            }
            "equals" => {
                // ToTemporalZonedDateTime casts the argument (string / property bag /
                // ZonedDateTime). Two ZonedDateTimes are equal iff their epoch-ns
                // match AND their time zones match (offset zones compared canonically,
                // so +00/+0000/+00:00 are equal) AND their calendars match (always
                // iso8601 here).
                let other_v = args.first().copied().unwrap_or(Value::UNDEFINED);
                let other = self.zoned_date_time_from(other_v, Value::UNDEFINED)?;
                let oi = other.heap_index();
                let eq = self.zdt_epoch_ns(idx) == self.zdt_epoch_ns(oi)
                    && self.tz_canon(idx) == self.tz_canon(oi)
                    && cal == self.cal_of(oi);
                Ok(Some(Value::bool(eq)))
            }
            "withTimeZone" => {
                // Same instant, different zone. A wrong-type zone is a TypeError.
                let (id, offset) =
                    self.parse_tz_arg(args.first().copied().unwrap_or(Value::UNDEFINED))?;
                let ns = self.zdt_epoch_ns(idx).unwrap_or(0);
                let r = self.alloc_zdt(ns, offset, id)?;
                Ok(Some(self.tag_cal(r, cal)))
            }
            "withCalendar" => {
                // Accept a calendar id / a calendar-bearing Temporal; reject a wrong
                // type (TypeError) or an unimplemented calendar (RangeError). The
                // argument is REQUIRED: undefined is a TypeError here.
                let cv = args.first().copied().unwrap_or(Value::UNDEFINED);
                if cv == Value::UNDEFINED {
                    return Err(Thrown("TypeError: withCalendar requires a calendar argument".into()));
                }
                let ncal = self.validate_calendar_value(cv)?;
                let (ns, off) = (self.zdt_epoch_ns(idx).unwrap_or(0), self.zdt_offset_ns(idx));
                let r = self.make_zoned_date_time_raw(ns, off, idx)?;
                Ok(Some(self.tag_cal(r, ncal)))
            }
            "withPlainTime" => {
                // No argument is GetStartOfDay, NOT midnight-disambiguated: in
                // America/Toronto on 1919-03-31 those differ by 30 minutes.
                let tv = args.first().copied().unwrap_or(Value::UNDEFINED);
                let time =
                    if tv == Value::UNDEFINED { None } else { Some(self.to_plain_time(tv)?) };
                let f = self.zdt_local(idx);
                let midnight = (iso_to_epoch_days(f[0], f[1], f[2]) as i128) * DAY_NS;
                let id = self.zdt_tz_id(idx).unwrap_or_else(|| "UTC".to_string());
                let ns = match time {
                    None => tz_start_of_day(&id, midnight)?,
                    Some(t) => tz_local_to_instant(&id, midnight + time_to_ns(&t), "compatible")?,
                };
                let off = tz_offset_ns_at(&id, ns);
                let r = self.alloc_zdt(ns, off, id)?;
                Ok(Some(self.tag_cal(r, cal)))
            }
            "add" | "subtract" => {
                // AddZonedDateTime: the calendar (year/month/week/day) part moves
                // in WALL-CLOCK space and is re-zoned, so adding a day across a
                // DST boundary keeps the clock time; the time part is then added
                // as exact duration on the timeline.
                let dur = self.to_duration(args.first().copied().unwrap_or(Value::UNDEFINED))?;
                let reject = self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                let sign: i64 = if name == "add" { 1 } else { -1 };
                let lf = self.zdt_local(idx);
                let id = self.zdt_tz_id(idx).unwrap_or_else(|| "UTC".to_string());
                let time_ns = ((dur[4] as i128) * 3_600_000_000_000
                    + (dur[5] as i128) * 60_000_000_000
                    + (dur[6] as i128) * 1_000_000_000
                    + (dur[7] as i128) * 1_000_000
                    + (dur[8] as i128) * 1_000
                    + (dur[9] as i128))
                    * sign as i128;
                let no_date_units = dur[..4].iter().all(|&x| x == 0);
                let result_ns = if no_date_units {
                    // Pure time: exact nanoseconds on the timeline, never re-zoned.
                    self.zdt_epoch_ns(idx).unwrap_or(0) + time_ns
                } else {
                    // The years/months step happens in CALENDAR space; the whole
                    // date part then lands back on the timeline through the zone
                    // (so "+1 day" over a spring-forward is 23 real hours), and
                    // the time part is added to THAT instant.
                    let (cy, cm, cd) = cal_from_iso(cal, lf[0], lf[1], lf[2]);
                    let (ay, am, ad) =
                        cal_add_year_month(cal, cy, cm, cd, dur[0] * sign, dur[1] * sign, reject)
                            .ok_or_else(|| {
                                Thrown("RangeError: date arithmetic overflows the month".into())
                            })?;
                    let ed = cal_to_epoch_days(cal, ay, am, ad) + (dur[2] * 7 + dur[3]) * sign;
                    let local = (ed as i128) * DAY_NS
                        + time_to_ns(&[lf[3], lf[4], lf[5], lf[6], lf[7], lf[8]]);
                    tz_local_to_instant(&id, local, "compatible")? + time_ns
                };
                // IsValidEpochNanoseconds: the result must lie within the supported
                // instant range (matches the ZonedDateTime/Instant constructors).
                if result_ns.abs() > NS_MAX_INSTANT {
                    return Err(Thrown(
                        "RangeError: ZonedDateTime result is outside the supported range".into(),
                    ));
                }
                let off = tz_offset_ns_at(&id, result_ns);
                let r = self.alloc_zdt(result_ns, off, id)?;
                Ok(Some(self.tag_cal(r, cal)))
            }
            "until" | "since" => {
                // DifferenceTemporalZonedDateTime. Default largestUnit is "hour".
                let other = args.first().copied().unwrap_or(Value::UNDEFINED);
                let oz = self.zoned_date_time_from(other, Value::UNDEFINED)?;
                if self.cal_of(oz.heap_index()) != cal {
                    return Err(Thrown(
                        "RangeError: cannot compute a difference between date-times in different calendars"
                            .into(),
                    ));
                }
                let opts = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                // GetOptionsObject: a defined non-object options bag is a TypeError
                // (a primitive must not be read for properties / silently ignored).
                if opts != Value::UNDEFINED && !self.is_object_value(opts) {
                    return Err(Thrown("TypeError: options must be an object or undefined".into()));
                }
                let all_units = &[
                    "auto", "year", "years", "month", "months", "week", "weeks", "day", "days",
                    "hour", "hours", "minute", "minutes", "second", "seconds", "millisecond",
                    "milliseconds", "microsecond", "microseconds", "nanosecond", "nanoseconds",
                ];
                let order = [
                    "year", "month", "week", "day", "hour", "minute", "second", "millisecond",
                    "microsecond", "nanosecond",
                ];
                let rank = |u: &str| order.iter().position(|&x| x == u).unwrap_or(9);
                // GetDifferenceSettings order: largestUnit, roundingIncrement, roundingMode,
                // smallestUnit — resolve "auto" + validate only after all four are read.
                let largest_raw =
                    normalize_unit(&self.opt_string(opts, "largestUnit", "auto", all_units)?, "auto");
                let inc = self.read_rounding_increment(opts)?;
                let mode = self.read_rounding_mode(opts, "trunc")?;
                let smallest = normalize_unit(
                    &self.opt_string(opts, "smallestUnit", "nanosecond", all_units)?,
                    "nanosecond",
                );
                let largest = if largest_raw == "auto" {
                    if rank(&smallest) < rank("hour") { smallest.clone() } else { "hour".to_string() }
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
                let eff = if name == "since" { negate_mode(&mode) } else { mode.clone() };
                let ns1 = self.zdt_epoch_ns(idx).unwrap_or(0);
                let ns2 = self.zdt_epoch_ns(oz.heap_index()).unwrap_or(0);
                let mut out = if rank(&largest) > rank("day") {
                    // A time largestUnit is DifferenceInstant: exact elapsed
                    // nanoseconds, so the two operands may sit in different zones.
                    balance_duration_ns(
                        round_increment(ns2 - ns1, unit_ns(&smallest) * inc, &eff),
                        &largest,
                    )?
                } else {
                    // A DATE largestUnit measures calendar days, which only means
                    // something if both operands keep the same clock: differencing
                    // across zones is a RangeError, and Asia/Calcutta vs
                    // Asia/Kolkata is the SAME zone (TimeZoneEquals compares
                    // primary identifiers).
                    if self.tz_canon(idx) != self.tz_canon(oz.heap_index()) {
                        return Err(Thrown(
                            "RangeError: cannot compute a calendar difference between ZonedDateTimes in different time zones"
                                .into(),
                        ));
                    }
                    if ns1 == ns2 {
                        return Ok(Some(self.make_duration([0.0; 10])));
                    }
                    let id = self.zdt_tz_id(idx).unwrap_or_else(|| "UTC".to_string());
                    diff_zoned_rounded(cal, &id, ns1, ns2, &largest, inc, &smallest, &eff)?
                };
                if name == "since" {
                    out.iter_mut().for_each(|x| *x = -*x);
                }
                Ok(Some(self.make_duration(out)))
            }
            "round" => {
                let opts = args.first().copied().unwrap_or(Value::UNDEFINED);
                let (su, inc, mode) = self.read_round_options(
                    opts,
                    &[
                        "day", "hour", "minute", "second", "millisecond", "microsecond",
                        "nanosecond",
                    ],
                    true,
                )?;
                let f = self.zdt_local(idx);
                let off = self.zdt_offset_ns(idx);
                let id = self.zdt_tz_id(idx).unwrap_or_else(|| "UTC".to_string());
                let day0 = (iso_to_epoch_days(f[0], f[1], f[2]) as i128) * DAY_NS;
                let ns = if su == "day" {
                    // Rounding to "day" measures against the REAL day boundaries
                    // (GetStartOfDay both ends, each of which must be a
                    // representable instant). Halfway through 2000-10-29 in
                    // Vancouver is 11:30, not 12:00, because that day is 25 hours
                    // long — and 1919-03-30 in Toronto does not even start at
                    // midnight, it starts at 00:30.
                    let start_ns = tz_start_of_day(&id, day0)?;
                    let end_ns = tz_start_of_day(&id, day0 + DAY_NS)?;
                    if start_ns.abs() > NS_MAX_INSTANT || end_ns.abs() > NS_MAX_INSTANT {
                        return Err(Thrown(
                            "RangeError: ZonedDateTime day boundary is outside the representable range"
                                .into(),
                        ));
                    }
                    // The answer is one of the two boundaries, never a multiple of
                    // the day length: in Antarctica/Casey on 2010-03-04 the clock
                    // went back three hours, so 23:10 on the SECOND 4 March is
                    // 26h10m into a 24-hour day — flooring that as an increment
                    // would step forward a whole day instead of down to the day's
                    // own start.
                    let epoch = self.zdt_epoch_ns(idx).unwrap_or(0);
                    if round_fraction_exact(0, 1, epoch - start_ns, end_ns - start_ns, &mode) == 0 {
                        start_ns
                    } else {
                        end_ns
                    }
                } else {
                    // RoundISODateTime on the wall clock, then
                    // InterpretISODateTimeOffset with the receiver's own offset and
                    // offset:"prefer" — inside a repeated hour that keeps the
                    // rounded time on the same side of the fold, and inside a gap
                    // (nothing carries that offset) it falls back to "compatible".
                    let time_ns = time_to_ns(&[f[3], f[4], f[5], f[6], f[7], f[8]]);
                    let rounded = round_increment(time_ns, unit_ns(&su) * inc, &mode);
                    let local = day0 + rounded;
                    interpret_iso_offset(&id, local, 2, off, "compatible", "prefer", false)?
                };
                let r = self.alloc_zdt(ns, tz_offset_ns_at(&id, ns), id)?;
                Ok(Some(self.tag_cal(r, cal)))
            }
            "with" => {
                // Merge date/time fields from the bag over the current local
                // wall-clock; the zone (and thus offset) is unchanged.
                let bag = args.first().copied().unwrap_or(Value::UNDEFINED);
                self.reject_temporal_like(bag)?;
                let mut f = self.zdt_local(idx);
                // PrepareCalendarFields reads ALPHABETICALLY: day, hour, microsecond,
                // millisecond, minute, month+monthCode, nanosecond, offset, second,
                // year — slot-mapped onto the local wall-clock [y,mo,d,h,mi,s,ms,us,ns].
                // The month slot goes through read_month_field_raw so monthCode is
                // honoured, month/monthCode agreement is enforced, and a calendar-
                // invalid code is deferred until after the options bag is read.
                let mut month_ref: Option<MonthRef> = None;
                let mut month_valid = true;
                let mut month_conflict = None;
                let mut any = false;
                // Which date slots the bag actually supplied — the date part must
                // merge in CALENDAR space, so the raw values cannot just overwrite
                // the ISO wall-clock.
                let mut date_raw: [Option<i64>; 3] = [None; 3];
                let mut read_slot = |vm: &mut Self, nm: &str, slot: usize, f: &mut [i64; 9], any: &mut bool, date_raw: &mut [Option<i64>; 3]| -> Result<(), Thrown> {
                    let v = if slot == 1 {
                        vm.read_month_field_raw(bag, cal)?.map(|(mm, valid, conflict)| {
                            month_valid = valid;
                            month_conflict = conflict;
                            month_ref = Some(mm);
                            mm.floor()
                        })
                    } else {
                        vm.opt_int_field(bag, nm)?
                    };
                    if let Some(x) = v {
                        if slot < 3 {
                            date_raw[slot] = Some(x);
                        } else {
                            f[slot] = x;
                        }
                        *any = true;
                    }
                    Ok(())
                };
                read_slot(self, "day", 2, &mut f, &mut any, &mut date_raw)?;
                let (era, era_year) = self.read_era_fields(bag, cal)?;
                any |= era.is_some() || era_year.is_some();
                read_slot(self, "hour", 3, &mut f, &mut any, &mut date_raw)?;
                read_slot(self, "microsecond", 7, &mut f, &mut any, &mut date_raw)?;
                read_slot(self, "millisecond", 6, &mut f, &mut any, &mut date_raw)?;
                read_slot(self, "minute", 4, &mut f, &mut any, &mut date_raw)?;
                read_slot(self, "month", 1, &mut f, &mut any, &mut date_raw)?;
                read_slot(self, "nanosecond", 8, &mut f, &mut any, &mut date_raw)?;
                // The bag's `offset` field sits at its alphabetical slot (after
                // nanosecond, before second); a bad string is a RangeError, a
                // non-string a TypeError. Its presence also satisfies the
                // "at least one recognized property" requirement.
                let bag_off = self.validate_bag_offset_field(bag)?;
                read_slot(self, "second", 5, &mut f, &mut any, &mut date_raw)?;
                read_slot(self, "year", 0, &mut f, &mut any, &mut date_raw)?;
                if !any && bag_off.is_none() {
                    return Err(Thrown(
                        "TypeError: with() requires at least one recognized property".into(),
                    ));
                }
                if era.is_some() != era_year.is_some() {
                    return Err(Thrown("TypeError: era and eraYear must be given together".into()));
                }
                // Merge the date in calendar space (NonIsoFieldKeysToIgnore: an
                // era+eraYear pair and `year` are mutually exclusive inputs).
                let (ccy, ccm, ccd) = cal_from_iso(cal, f[0], f[1], f[2]);
                let ny = if era.is_some() || date_raw[0].is_some() {
                    Self::resolve_cal_year(cal, era.as_deref(), era_year, date_raw[0])?
                } else {
                    ccy
                };
                // No month field keeps the receiver's MONTH CODE, not its ordinal
                // (they differ across a leap-month calendar's year boundary).
                let nm = month_ref.unwrap_or(MonthRef::of(cal, ccy, ccm));
                let nd = date_raw[2].unwrap_or(ccd);
                // month/day use ToPositiveIntegerWithTruncation: a value below 1 is
                // rejected during field preparation, BEFORE the options bag is read.
                if nm.floor() < 1 || nd < 1 {
                    return Err(Thrown("RangeError: invalid date fields".into()));
                }
                // Validate the resolution options. ZonedDateTime.with defaults the offset
                // option to "prefer" (unlike `from`, which defaults to "reject").
                let options = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let (off_opt, disamb, reject) = self.read_zdt_options(options, "prefer")?;
                // A month/monthCode conflict, or a well-formed-but-calendar-invalid
                // monthCode ("M08L", "M13"), is rejected only after the options bag.
                if !month_valid {
                    return Err(Thrown(format!(
                        "RangeError: monthCode is not valid for the {} calendar",
                        cal.id()
                    )));
                }
                if !cal_month_fields_agree(cal, ny, nm, month_conflict) {
                    return Err(Thrown("RangeError: month and monthCode must agree".into()));
                }
                // InterpretTemporalDateTimeFields: apply overflow to the upper bounds of
                // the merged date/time fields ("reject" throws, "constrain" clamps).
                let maxes = [23, 59, 59, 999, 999, 999];
                let (iy, im, id2) = nm
                    .ordinal(cal, ny, reject)
                    .and_then(|m| cal_date_to_iso(cal, ny, m, nd, reject))
                    .ok_or_else(|| Thrown("RangeError: invalid date fields".into()))?;
                f[0] = iy;
                f[1] = im;
                f[2] = id2;
                if reject {
                    for (i, &mx) in maxes.iter().enumerate() {
                        if f[3 + i] < 0 || f[3 + i] > mx {
                            return Err(Thrown("RangeError: time field out of range".into()));
                        }
                    }
                } else {
                    for (i, &mx) in maxes.iter().enumerate() {
                        f[3 + i] = f[3 + i].clamp(0, mx);
                    }
                }
                // Offset agreement (InterpretISODateTimeOffset). `with` carries the
                // RECEIVER's offset forward when the bag did not supply one, which
                // is what makes the default "prefer" keep the same side of a
                // fall-back repeat instead of silently jumping to the earlier one.
                let id = self.zdt_tz_id(idx).unwrap_or_else(|| "UTC".to_string());
                let merged_off = bag_off.unwrap_or_else(|| self.zdt_offset_ns(idx));
                let local = (iso_to_epoch_days(f[0], f[1], f[2]) as i128) * DAY_NS
                    + time_to_ns(&[f[3], f[4], f[5], f[6], f[7], f[8]]);
                let instant =
                    interpret_iso_offset(&id, local, 2, merged_off, &disamb, &off_opt, false)?;
                // The resulting instant must be representable (the ±nsMaxInstant bound).
                if instant.abs() > NS_MAX_INSTANT {
                    return Err(Thrown(
                        "RangeError: ZonedDateTime outside the supported range".into(),
                    ));
                }
                let off = tz_offset_ns_at(&id, instant);
                let r = self.alloc_zdt(instant, off, id)?;
                Ok(Some(self.tag_cal(r, cal)))
            }
            _ => Ok(None),
        }
    }

    /// Build a ZonedDateTime from epoch ns + offset, copying the time-zone id of an
    /// existing instance `src` (used by methods that derive a new ZDT in place).
    pub(crate) fn make_zoned_date_time_raw(&mut self, ns: i128, offset_ns: i64, src: u32) -> Result<Value, Thrown> {
        if ns.abs() > NS_MAX_INSTANT {
            return Err(Thrown(
                "RangeError: ZonedDateTime is outside the representable range".into(),
            ));
        }
        let _gc = self.gc_lock_guard();
        let hi = (ns >> 64) as i64;
        let lo = ns as i64;
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 7, fields: vec![hi, lo, offset_ns] });
        if self.zoneddatetime_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.zoneddatetime_proto));
        }
        if let Some(tz) = self.zdt_tz.get(&src).copied() {
            self.zdt_tz.insert(idx, tz);
        }
        // The derived instance keeps the source's calendar (withCalendar re-tags).
        if let Some(cal) = self.temporal_cal.get(&src).copied() {
            self.temporal_cal.insert(idx, cal);
        }
        Ok(Value::heap(idx))
    }

    /// `Temporal.ZonedDateTime.from(item[, options])`. From a ZDT instance → a copy;
    /// from a property bag `{timeZone, year, month, day, …}` → built from the local
    /// wall-clock + zone offset; from an ISO string `…±OFF[tz]` → parsed.
    pub(crate) fn zoned_date_time_from(&mut self, item: Value, options: Value) -> Result<Value, Thrown> {
        let _gc = self.gc_lock_guard();
        if item.is_heap() {
            if let Some(ns) = self.zdt_epoch_ns(item.heap_index()) {
                let off = self.zdt_offset_ns(item.heap_index());
                // The disambiguation/offset/overflow options are validated even for a
                // ZonedDateTime instance (the result is still a copy).
                let _ = self.read_zdt_options(options, "reject")?;
                return Ok(self.make_zoned_date_time_raw(ns, off, item.heap_index())?);
            }
            if self.is_object_value(item)
                && !matches!(self.heap.get(item.heap_index()), HeapObj::Temporal { .. })
            {
                // ToTemporalZonedDateTime: ONE PrepareCalendarFields pass (a Proxy
                // bag included — the previous HeapObj::Object gate bounced those)
                // reads calendar then the date/time fields alphabetically with
                // offset and timeZone at their slots; then the timeZone-required
                // TypeError; then the disambiguation/offset/overflow options; then
                // InterpretTemporalDateTimeFields.
                let bag = self.read_pdt_bag(item, true)?;
                if bag.tz == Value::UNDEFINED {
                    return Err(Thrown(
                        "TypeError: Temporal.ZonedDateTime.from requires a timeZone property".into(),
                    ));
                }
                // ToTemporalTimeZoneIdentifier: a string is parsed, a wrong type
                // (null/boolean/number/bigint/symbol) is a TypeError — not coerced.
                let (id, _) = self.parse_tz_arg(bag.tz)?;
                let (off_opt, disamb, reject) = self.read_zdt_options(options, "reject")?;
                let cal = bag.cal;
                let f = self.finish_pdt_fields(&bag, reject)?;
                let local = (iso_to_epoch_days(f[0], f[1], f[2]) as i128) * DAY_NS
                    + time_to_ns(&[f[3], f[4], f[5], f[6], f[7], f[8]]);
                // A bag with no `offset` is WALL (the zone decides, disambiguation
                // resolves gaps/repeats); with one it is OPTION and must be
                // reconciled. A BAG offset must match to the nanosecond — only a
                // parsed STRING gets the MATCH-MINUTES latitude.
                let behaviour = if bag.bag_off.is_some() { 2 } else { 0 };
                let ns = interpret_iso_offset(
                    &id,
                    local,
                    behaviour,
                    bag.bag_off.unwrap_or(0),
                    &disamb,
                    &off_opt,
                    false,
                )?;
                let off = tz_offset_ns_at(&id, ns);
                let r = self.alloc_zdt(ns, off, id)?;
                return Ok(self.tag_cal(r, cal));
            }
        }
        // An Object was handled above; only a String is parseable. Any other value —
        // a non-string primitive (number/bigint/boolean/null/undefined) or a non-string
        // heap value (Symbol) — is a TypeError per ToTemporalZonedDateTime, NOT a failed
        // string parse (RangeError). This also precedes reading the options bag.
        if !(item.is_heap() && self.heap.is_str_like(item.heap_index())) {
            return Err(Thrown(
                "TypeError: ZonedDateTime.from argument must be an object or string".into(),
            ));
        }
        let s = self.to_js_string(item)?;
        // The string is parsed/validated (RangeError on a bad string) BEFORE the
        // options bag is read — so `from("bad-string", primitiveOptions)` is a
        // RangeError, not the options TypeError.
        if !temporal_string_ok(&s, false, true) {
            return Err(Thrown(format!("RangeError: invalid ZonedDateTime string \"{s}\"")));
        }
        let cal = self.calendar_from_annotation(&s)?;
        let (f, str_offset, id, _zone_offset, behaviour) = parse_zdt_string(&s)
            .ok_or_else(|| Thrown(format!("RangeError: invalid ZonedDateTime string \"{s}\"")))?;
        let (off_opt, disamb, _reject) = self.read_zdt_options(options, "reject")?;
        // InterpretISODateTimeOffset step 6 (OPTION behaviour, offset prefer/
        // reject): CheckISODaysRange rejects a WALL date beyond ±10^8 epoch days
        // even when the resulting instant is exactly representable (e.g.
        // '-271821-04-19T23:00-01:00[-01:00]' has epoch == nsMin but wall date
        // -271821-04-19). Never applies to Z/wall behaviours or use/ignore.
        if behaviour >= 2
            && matches!(off_opt.as_str(), "prefer" | "reject")
            && iso_to_epoch_days(f[0], f[1], f[2]).abs() > 100_000_000
        {
            return Err(Thrown(
                "RangeError: ZonedDateTime is outside the representable range".into(),
            ));
        }
        let local = (iso_to_epoch_days(f[0], f[1], f[2]) as i128) * DAY_NS
            + time_to_ns(&[f[3], f[4], f[5], f[6], f[7], f[8]]);
        // A DATE-ONLY string is GetStartOfDay, not midnight: "1919-03-31
        // [America/Toronto]" is 00:30, the instant the skipped hour ended, while
        // "1919-03-31T00[America/Toronto]" disambiguates to 01:00.
        let head = s.trim().split('[').next().unwrap_or("");
        if behaviour == 0 && !head.contains(['T', 't', ' ']) {
            let r = tz_start_of_day(&id, local)?;
            let v = self.alloc_zdt(r, tz_offset_ns_at(&id, r), id)?;
            return Ok(self.tag_cal(v, cal));
        }
        // Offset agreement (InterpretISODateTimeOffset). A parsed string gets the
        // MATCH-MINUTES rule: `…-04:56[America/New_York]` written against the
        // 1883 LMT offset of −04:56:02 still matches, because the ISO grammar
        // cannot spell the seconds.
        let ns =
            // MATCH-MINUTES only for a minute-precision offset (behaviour 2).
            interpret_iso_offset(&id, local, behaviour.min(2) as u8, str_offset, &disamb,
                                 &off_opt, behaviour == 2)?;
        let r = self.alloc_zdt(ns, tz_offset_ns_at(&id, ns), id)?;
        Ok(self.tag_cal(r, cal))
    }

    /// Allocate a ZonedDateTime from epoch ns, offset, and an (owned) tz id.
    /// IsValidEpochNanoseconds: an out-of-range instant is a RangeError (so every
    /// ZDT-producing path is guarded centrally, not per-caller).
    pub(crate) fn alloc_zdt(&mut self, ns: i128, offset_ns: i64, id: String) -> Result<Value, Thrown> {
        if ns.abs() > NS_MAX_INSTANT {
            return Err(Thrown(
                "RangeError: ZonedDateTime is outside the representable range".into(),
            ));
        }
        let hi = (ns >> 64) as i64;
        let lo = ns as i64;
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 7, fields: vec![hi, lo, offset_ns] });
        if self.zoneddatetime_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.zoneddatetime_proto));
        }
        let idv = self.alloc_str(id);
        self.zdt_tz.insert(idx, idv);
        Ok(Value::heap(idx))
    }

    /// Resolve a time-zone argument (a string id, or an object with a `timeZone`
    /// property) into a (normalized id, offset-ns) pair.
    pub(crate) fn parse_tz_arg(&mut self, v: Value) -> Result<(String, i64), Thrown> {
        // A `{ timeZone }` bag yields its timeZone field; otherwise the value is
        // itself the time-zone-like.
        let tz = if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
            let t = self.get_prop(v, "timeZone")?;
            if t == Value::UNDEFINED {
                return Err(Thrown("TypeError: a timeZone is required".into()));
            }
            t
        } else {
            v
        };
        // ToTemporalTimeZoneIdentifier: a ZonedDateTime carries its zone, a string
        // is parsed; anything else (incl. undefined and plain objects) is a
        // TypeError.
        if tz.is_heap() {
            if let HeapObj::Temporal { kind: 7, .. } = self.heap.get(tz.heap_index()) {
                let id = self.zdt_tz_id(tz.heap_index()).unwrap_or_else(|| "UTC".to_string());
                return parse_time_zone(&id)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid time zone \"{id}\"")));
            }
            if self.heap.is_str_like(tz.heap_index()) {
                let s = self.heap.str_cow(tz.heap_index()).unwrap().into_owned();
                return parse_time_zone(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid time zone \"{s}\"")));
            }
        }
        Err(Thrown("TypeError: timeZone is not a string or object".into()))
    }

    /// `Temporal.Now.*` time-zone argument: `undefined` is the system zone (UTC
    /// here, no tz database), otherwise validate it via ToTemporalTimeZoneIdentifier
    /// (a string id or a ZonedDateTime; anything else throws). Returns (id, offset-ns).
    pub(crate) fn now_tz_id(&mut self, arg: Value) -> Result<(String, i64), Thrown> {
        if arg == Value::UNDEFINED {
            return Ok(("UTC".to_string(), 0));
        }
        self.parse_tz_arg(arg)
    }

    /// GetDirectionOption (ZonedDateTime.getTimeZoneTransition): the arg is a
    /// "next"/"previous" string, or an options bag whose `direction` is one of
    /// those. A non-string/non-object arg -> TypeError; a missing direction ->
    /// RangeError; a non-"next"/"previous" string -> RangeError; a Symbol value
    /// -> TypeError (via ToString).
    pub(crate) fn read_direction_option(&mut self, arg: Value) -> Result<String, Thrown> {
        let dir_v = if arg.is_heap() && self.heap.is_str_like(arg.heap_index()) {
            arg
        } else if self.is_object_value(arg) {
            self.get_prop(arg, "direction")?
        } else {
            return Err(Thrown(
                "TypeError: direction must be a string or an options object".into(),
            ));
        };
        if dir_v == Value::UNDEFINED {
            return Err(Thrown("RangeError: a direction option is required".into()));
        }
        let s = self.to_js_string(dir_v)?;
        if s != "next" && s != "previous" {
            return Err(Thrown(format!("RangeError: invalid direction option '{s}'")));
        }
        Ok(s)
    }

    /// Resolve a `relativeTo` option to its anchor: the wall-clock fields, the
    /// ZONE IDENTIFIER when it is ZonedDateTime-like (a ZDT instance, a
    /// `[tz]`-annotated string, or a bag carrying a `timeZone`) and `None` when
    /// it is plain, and the zone OFFSET pairing with those wall fields
    /// (epoch = wall − offset; 0 for plain anchors). The identifier — not just
    /// the offset — is what the zoned duration machinery needs: the offset at
    /// the anchor says nothing about the offset a month later. A PLAIN anchor is
    /// a PlainDate per GetTemporalRelativeToOption, so the time-of-day of a
    /// datetime string / PlainDateTime / non-zoned bag is dropped (midnight).
    /// Zoned string anchors are validated here: the epoch must be a representable
    /// instant, and an explicit-offset string is additionally subject to
    /// CheckISODaysRange on its wall date.
    pub(crate) fn relative_to_dt(
        &mut self,
        rel: Value,
    ) -> Result<([i64; 9], Option<String>, i64, Cal), Thrown> {
        if rel.is_heap() {
            if matches!(self.heap.get(rel.heap_index()), HeapObj::Temporal { kind: 7, .. }) {
                let idx = rel.heap_index();
                let cal = self.cal_of(idx);
                let id = self.zdt_tz_id(idx).unwrap_or_else(|| "UTC".to_string());
                return Ok((self.zdt_local(idx), Some(id), self.zdt_offset_ns(idx), cal));
            }
            // A property bag is read in ONE PrepareCalendarFields pass (calendar,
            // then the fields alphabetically with offset and timeZone at their
            // slots — the old code peeked timeZone FIRST and never read offset).
            // A bag carrying a timeZone is ZonedDateTime-like: the zone is
            // validated, and the wall-clock date/time is the anchor. Temporal
            // instances (PlainDate/PlainDateTime) fall through to the lenient
            // conversion below, which reads no observable properties on them.
            if self.is_object_value(rel)
                && !matches!(self.heap.get(rel.heap_index()), HeapObj::Temporal { .. })
            {
                let bag = self.read_pdt_bag(rel, true)?;
                let zoned = bag.tz != Value::UNDEFINED;
                let id = if zoned { self.parse_tz_arg(bag.tz)?.0 } else { String::new() };
                let cal = bag.cal;
                let mut f = self.finish_pdt_fields(&bag, false)?;
                if !zoned {
                    f[3..9].fill(0);
                }
                // The anchor's offset is the zone's offset AT the anchor instant,
                // not a per-zone constant — a relativeTo in July and one in
                // January differ by an hour in half the world. A bag `offset`
                // resolves with offset "reject" and NO fuzzy minute matching
                // (that latitude is only for parsed strings).
                let mut off = 0i64;
                if zoned {
                    let local = dt_epoch_ns(f);
                    let ns = match bag.bag_off {
                        Some(b) => {
                            interpret_iso_offset(&id, local, 2, b, "compatible", "reject", false)?
                        }
                        None => tz_local_to_instant(&id, local, "compatible")?,
                    };
                    off = (local - ns) as i64;
                }
                return Ok((f, zoned.then_some(id), off, cal));
            }
        }
        // A plain STRING relativeTo (ToRelativeTemporalObject) uses a LOOSER grammar
        // than PlainDateTime parsing: a `Z` UTC designator is allowed WHEN a time-zone
        // annotation is present, and a bare date may carry a `[tz]` annotation. Strip
        // the annotation, validate it (Z not blanket-rejected), enforce the
        // Z-requires-tz-annotation rule, then field-parse the annotation-stripped main.
        if rel.is_heap() && self.heap.is_str_like(rel.heap_index()) {
            let s = self.heap.str_cow(rel.heap_index()).unwrap().into_owned();
            let st = s.trim();
            let (main, ann) = match st.find('[') {
                Some(i) => (&st[..i], &st[i..]),
                None => (st, ""),
            };
            if !temporal_string_ok(st, false, true) {
                return Err(Thrown(format!("RangeError: invalid datetime string '{s}'")));
            }
            // A time-zone annotation is any `[...]` block whose body (after an optional
            // leading `!`) has no `=` (so [UTC]/[-07:00]/[!UTC] count; [u-ca=…] does not).
            let has_tz_ann = ann
                .split(['[', ']'])
                .any(|b| !b.is_empty() && !b.trim_start_matches('!').contains('='));
            if main.bytes().any(|b| b == b'Z' || b == b'z') && !has_tz_ann {
                return Err(Thrown(format!("RangeError: invalid datetime string '{s}'")));
            }
            // With a time-zone annotation the relativeTo is ZonedDateTime-like. Per
            // ToRelativeTemporalObject (offset:"reject") only an EXPLICIT offset that
            // disagrees with the zone is a RangeError; a `Z` designator (EXACT) is the
            // exact UTC instant and is accepted, and no offset (WALL) uses the zone. The
            // wall-clock fields are the anchor.
            if has_tz_ann {
                let (f, str_offset, id, _zone_offset, behaviour) = parse_zdt_string(st)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid datetime string '{s}'")))?;
                // ToRelativeTemporalObject resolves with offset "reject": an
                // explicit offset that no instant in the zone actually has is a
                // RangeError. The wall fields then pair with the offset the
                // resolved instant really carries.
                let eff = (dt_epoch_ns(f)
                    - interpret_iso_offset(
                        &id,
                        dt_epoch_ns(f),
                        behaviour.min(2) as u8,
                        str_offset,
                        "compatible",
                        "reject",
                        behaviour == 2,
                    )
                    .map_err(|_| {
                        Thrown(format!(
                            "RangeError: the relativeTo offset does not match the time zone in '{s}'"
                        ))
                    })?) as i64;
                // The anchor must be a representable instant, and an explicit-
                // offset string is subject to CheckISODaysRange on its WALL date
                // (relativeTo resolves with offset "reject") even when the epoch
                // is exactly at the bound.
                if (dt_epoch_ns(f) - eff as i128).abs() > NS_MAX_INSTANT
                    || (behaviour >= 2 && iso_to_epoch_days(f[0], f[1], f[2]).abs() > 100_000_000)
                {
                    return Err(Thrown(format!(
                        "RangeError: relativeTo '{s}' is outside the representable range"
                    )));
                }
                return Ok((f, Some(id), eff, self.calendar_from_annotation(st)?));
            }
            let mut f = parse_iso_datetime(main)
                .ok_or_else(|| Thrown(format!("RangeError: invalid datetime string '{s}'")))?;
            // GetTemporalRelativeToOption yields a PlainDate: the time-of-day is
            // dropped, and the date itself must be in the ISO range.
            f[3..9].fill(0);
            if !iso_date_in_range(f[0], f[1], f[2]) {
                return Err(Thrown(format!(
                    "RangeError: relativeTo '{s}' is outside the representable range"
                )));
            }
            return Ok((f, None, 0, self.calendar_from_annotation(st)?));
        }
        // Temporal instances (PlainDate/PlainDateTime) and other coercibles:
        // a plain anchor is a PlainDate, so any time-of-day is dropped.
        let (mut f, cal) = self.to_plain_date_time_cal(rel)?;
        f[3..9].fill(0);
        Ok((f, None, 0, cal))
    }

    /// Validate a ZonedDateTime-like property bag's `offset` field: if present it
    /// must be a well-formed UTC-offset string (`±HH:MM…`). (The offset-vs-time-zone
    /// agreement check needs a tz database and is not done here.)
    /// Validate a ZonedDateTime-like bag's `offset` field and return its value in
    /// nanoseconds (`None` if absent). The offset must be a well-formed UTC-offset
    /// string; whether it must AGREE with the time zone is decided by the caller via
    /// the `offset` option.
    pub(crate) fn validate_bag_offset_field(&mut self, bag: Value) -> Result<Option<i64>, Thrown> {
        let offv = self.get_prop(bag, "offset")?;
        if offv == Value::UNDEFINED {
            return Ok(None);
        }
        // The offset must be a String or an object (which ToString-s); a primitive
        // non-string (null/boolean/number/bigint/symbol) is a TypeError.
        let is_string = offv.is_heap() && self.heap.is_str_like(offv.heap_index());
        if !is_string && !self.is_object_value(offv) {
            return Err(Thrown("TypeError: offset must be a string".into()));
        }
        let offs = self.to_js_string(offv)?;
        if !valid_offset_string(&offs) {
            return Err(Thrown(format!("RangeError: invalid offset string \"{offs}\"")));
        }
        Ok(parse_offset_ns(&offs).map(|n| n as i64))
    }

    /// `Temporal.Duration.compare(one, two, { relativeTo })`. With a relativeTo
    /// anchor both durations are added to it and their resulting instants are
    /// compared (correct for years/months/weeks). Without one, calendar units
    /// are a RangeError and the remaining day+time span is compared directly.
    pub(crate) fn duration_compare(
        &mut self,
        fa: [f64; 10],
        fb: [f64; 10],
        opts: Value,
    ) -> Result<f64, Thrown> {
        // GetOptionsObject: a non-undefined options must be an object.
        if opts != Value::UNDEFINED && !self.is_object_value(opts) {
            return Err(Thrown("TypeError: options must be an object or undefined".into()));
        }
        let rel = if opts == Value::UNDEFINED {
            Value::UNDEFINED
        } else {
            self.get_prop(opts, "relativeTo")?
        };
        let order = |a: i128, b: i128| if a < b { -1.0 } else if a > b { 1.0 } else { 0.0 };
        // GetTemporalRelativeToOption parses/validates the relativeTo (throwing on an
        // invalid string) BEFORE the identical-slots short-circuit below.
        let start =
            if rel != Value::UNDEFINED { Some(self.relative_to_dt(rel)?) } else { None };
        // Two durations with identical internal slots compare equal (+0) — even with
        // calendar units and no relativeTo (the relativeTo requirement is skipped).
        if fa == fb {
            return Ok(0.0);
        }
        // When NEITHER duration has a date unit (largest unit below "day"), the
        // comparison is a pure time comparison: the resolved relativeTo (whose
        // parse errors above still throw) is never anchored against, so a
        // boundary anchor must not make a 5-minutes-vs-blank compare throw.
        if fa[..4].iter().all(|&x| x == 0.0) && fb[..4].iter().all(|&x| x == 0.0) {
            return Ok(order(dur_day_time_ns(&fa), dur_day_time_ns(&fb)));
        }
        if let Some((start, tz, off, cal)) = start {
            // A ZONED anchor moves each duration through the real zone: 1 day
            // and 24 hours from the same instant land an hour apart across a
            // DST change, so comparing them in wall-clock space calls them equal.
            if let Some(id) = tz {
                let ns = dt_epoch_ns(start) - off as i128;
                let e1 = add_zoned(cal, &id, ns, &fa)?;
                let e2 = add_zoned(cal, &id, ns, &fb)?;
                return Ok(order(e1, e2));
            }
            // Both anchored end-points must be representable (lenient on the
            // plain start: compare uses day-granular date arithmetic).
            check_relative_target(cal, start, &fa, false, off, false)?;
            check_relative_target(cal, start, &fb, false, off, false)?;
            let e1 = dur_end_epoch_ns(cal, start, &fa);
            let e2 = dur_end_epoch_ns(cal, start, &fb);
            return Ok(order(e1, e2));
        }
        if fa[..3].iter().any(|&x| x != 0.0) || fb[..3].iter().any(|&x| x != 0.0) {
            return Err(Thrown(
                "RangeError: a relativeTo option is required for years, months, or weeks".into(),
            ));
        }
        Ok(order(dur_day_time_ns(&fa), dur_day_time_ns(&fb)))
    }

    /// `Duration.round` with a PLAIN relativeTo anchor: round the span `start →
    /// start+duration` exactly like `PlainDateTime.prototype.until`, so the
    /// calendar-unit nudging, the day/time remainder rounding (time-of-day included
    /// via epoch nanoseconds), and the re-balance to largestUnit are all shared.
    /// (A ZONED anchor goes through `diff_zoned_rounded` instead — none of the
    /// day lengths here would be right.)
    pub(crate) fn round_duration_relative(
        &mut self,
        cal: Cal,
        f: [i64; 10],
        start: [i64; 9],
        smallest: &str,
        largest: &str,
        inc: i128,
        mode: &str,
    ) -> Result<[i64; 10], Thrown> {
        let end = dt_add_dur(cal, start, f);
        let order = [
            "year", "month", "week", "day", "hour", "minute", "second", "millisecond",
            "microsecond", "nanosecond",
        ];
        let rank = |u: &str| order.iter().position(|&x| x == u).unwrap_or(9);
        if rank(largest) >= rank("day") {
            // A day-or-time largestUnit is a pure nanosecond span: round it, balance.
            let total_ns = dt_epoch_ns(end) - dt_epoch_ns(start);
            let rounded = round_increment(total_ns, unit_ns(smallest) * inc, mode);
            Ok(balance_duration_ns(rounded, largest)?.map(Self::dur_to_i64))
        } else if matches!(smallest, "year" | "month" | "week") {
            // Calendar largestUnit + calendar smallestUnit → NudgeToCalendarUnit.
            round_relative_datetime_diff(cal, start, end, smallest, largest, inc, mode)
        } else {
            // Calendar largestUnit + day/time smallestUnit → round the day+time
            // remainder and roll an overflowing day up into the calendar units.
            let df = difference_datetime_cal(cal, start, end, largest);
            Ok(round_datetime_diff_daytime(cal, start, df, smallest, largest, inc, mode))
        }
    }

    /// The time-zone id string of a ZDT instance (for equality).
    pub(crate) fn zdt_tz_id(&self, idx: u32) -> Option<String> {
        self.zdt_tz
            .get(&idx)
            .and_then(|v| self.heap.str_cow(v.heap_index()).map(|s| s.into_owned()))
    }

    /// TimeZoneEquals: an offset zone collapses to its formatted offset (so
    /// "+00"/"+0000"/"+00:00" all match), a named zone to its PRIMARY
    /// identifier — which is why `Asia/Calcutta` equals `Asia/Kolkata` and
    /// `Etc/GMT` equals `UTC`.
    pub(crate) fn tz_canon(&self, idx: u32) -> String {
        let id = self.zdt_tz_id(idx).unwrap_or_else(|| "UTC".to_string());
        if id.starts_with(['+', '-']) {
            return format_offset(self.zdt_offset_ns(idx));
        }
        match tzdb::lookup(&id) {
            Some(z) => tzdb::primary(z.zone).to_string(),
            None => id,
        }
    }

    /// ISO string for a ZonedDateTime: `YYYY-MM-DDTHH:MM:SS<offset>[<tzid>]`.
    pub(crate) fn zdt_to_string(&self, idx: u32) -> String {
        let f = self.zdt_local(idx);
        let off = self.zdt_offset_ns(idx);
        let offset = format_offset_rounded(off);
        let tz = self
            .zdt_tz
            .get(&idx)
            .and_then(|v| self.heap.str_cow(v.heap_index()).map(|s| s.into_owned()))
            .unwrap_or_else(|| "UTC".to_string());
        let mut frac = String::new();
        let sub = f[6] * 1_000_000 + f[7] * 1_000 + f[8];
        if sub != 0 {
            frac = format!(".{:09}", sub);
            while frac.ends_with('0') {
                frac.pop();
            }
        }
        format!(
            "{}T{:02}:{:02}:{:02}{}{}[{}]",
            iso_date_string(f[0], f[1], f[2]),
            f[3], f[4], f[5], frac, offset, tz
        )
    }

    /// ZonedDateTime toString honouring options: smallestUnit/fractionalSecondDigits
    /// rounding of the instant + roundingMode, the calendarName suffix, plus the
    /// `offset` ("auto"/"never") and `timeZoneName` ("auto"/"never"/"critical")
    /// suffixes. Order: `<date>T<time><offset>[tz][u-ca=…]`.
    pub(crate) fn zdt_to_string_opts(&mut self, idx: u32, options: Value) -> Result<String, Thrown> {
        // All six options are read in ALPHABETICAL order — calendarName,
        // fractionalSecondDigits, offset, roundingMode, smallestUnit,
        // timeZoneName — and only then is the smallestUnit unit-CLASS
        // validated, so timeZoneName is observably read before the date-unit
        // RangeError (options-read-before-algorithmic-validation).
        let (unit, digits, omit, mode, cal_suf, show_offset, tzn) =
            if options == Value::UNDEFINED {
                // `showCalendar` still defaults to "auto", which annotates every
                // NON-ISO calendar — hard-coding "" here dropped `[u-ca=gregory]`
                // from `zdt.toString()` and `zdt.toString(undefined)` while
                // `zdt.toString({})` (which takes the branch below) kept it
                // (ZonedDateTime/prototype/toString/options-undefined.js).
                let cal_suf = self.calendar_name_suffix(Value::UNDEFINED, self.cal_of(idx))?;
                (1, -1, false, "trunc".to_string(), cal_suf, true, "auto".to_string())
            } else {
                if !self.is_object_value(options) {
                    return Err(Thrown("TypeError: options must be an object or undefined".into()));
                }
                let cal_suf = self.calendar_name_suffix(options, self.cal_of(idx))?;
                let fsd = self.read_fsd(options)?;
                let off_opt = self.opt_string(options, "offset", "auto", &["auto", "never"])?;
                let mode = self.read_rounding_mode_opt(options)?;
                let su = self.read_tostring_unit_token(options)?;
                let tzn = self.opt_string(
                    options,
                    "timeZoneName",
                    "auto",
                    &["auto", "never", "critical"],
                )?;
                let (unit, digits, omit) = Self::tostring_precision(su.as_deref(), fsd)?;
                (unit, digits, omit, mode, cal_suf, off_opt != "never", tzn)
            };
        // Round the instant to the requested unit, then express in the offset.
        // Rounding is on the ABSOLUTE timeline (epoch ns), so it rounds as-if the
        // value were positive (like Instant.toString) — NOT sign-relative, which
        // would round a negative epoch the wrong way for expand/ceil/floor.
        let epoch = self.zdt_epoch_ns(idx).unwrap_or(0);
        let rounded = round_increment_as_if_positive(epoch, unit, &mode);
        // The wall clock and offset belong to the ROUNDED instant, not the
        // original: rounding 2000-04-02T01:59:59.999999999-08 up to eight
        // fractional digits in America/Vancouver crosses the spring-forward, so
        // the answer is 03:00:00.00-07:00, not 02:00:00.00-08:00.
        let (tz, off) = match self.zdt_tz_id(idx) {
            Some(id) => {
                let o = tz_offset_ns_at(&id, rounded);
                (id, o)
            }
            None => ("UTC".to_string(), self.zdt_offset_ns(idx)),
        };
        let local = rounded + off as i128;
        let t = ns_to_time(local.rem_euclid(DAY_NS));
        let (ny, nm, nd) = epoch_days_to_iso(local.div_euclid(DAY_NS) as i64);
        let offset_s = if show_offset { format_offset_rounded(off) } else { String::new() };
        let tz_suf = match tzn.as_str() {
            "never" => String::new(),
            "critical" => format!("[!{tz}]"),
            _ => format!("[{tz}]"),
        };
        Ok(format!(
            "{}T{}{}{}{}",
            iso_date_string(ny, nm, nd),
            format_time_part(&t, digits, omit),
            offset_s,
            tz_suf,
            cal_suf
        ))
    }

    pub(crate) fn to_instant_ns(&mut self, v: Value) -> Result<i128, Thrown> {
        if v.is_heap() {
            if let Some(ns) = self.instant_ns(v.heap_index()) {
                return Ok(ns);
            }
            // A ZonedDateTime yields its epoch nanoseconds.
            if matches!(self.heap.get(v.heap_index()), HeapObj::Temporal { kind: 7, .. }) {
                if let Some(ns) = self.zdt_epoch_ns(v.heap_index()) {
                    return Ok(ns);
                }
            }
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                return self.parse_instant_string(&s);
            }
            // Any other object: ToPrimitive(string) then parse as an instant string
            // (e.g. {} -> "[object Object]" -> RangeError; a custom toString is honoured).
            if self.is_object_value(v) {
                let s = self.to_js_string(v)?;
                return self.parse_instant_string(&s);
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.Instant".into()))
    }

    pub(crate) fn parse_instant_string(&mut self, s: &str) -> Result<i128, Thrown> {
        if !temporal_string_ok(s, false, false) {
            return Err(Thrown(format!("RangeError: invalid instant string '{s}'")));
        }
        let ns = instant_str_to_ns(s)
            .ok_or_else(|| Thrown(format!("RangeError: invalid instant string '{s}'")))?;
        // IsValidEpochNanoseconds: compare/equals/since/until reach the ns directly
        // (only make_instant via from() range-checks otherwise).
        if ns.abs() > NS_MAX_INSTANT {
            return Err(Thrown(format!("RangeError: instant '{s}' is outside the supported range")));
        }
        Ok(ns)
    }

    pub(crate) fn instant_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let ns = match self.instant_ns(idx) {
            Some(n) => n,
            None => return Ok(None),
        };
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "toJSON" => Ok(Some(self.alloc_str(instant_to_string(ns)))),
            "toString" => {
                // Options read alphabetically — fractionalSecondDigits, roundingMode,
                // smallestUnit (token), timeZone — with the smallestUnit unit-CLASS
                // validated only after the timeZone get (the 2025 normative
                // options-read-before-algorithmic-validation ordering).
                let (fsd, mode, su) = if a0 == Value::UNDEFINED {
                    ((1, -1, false), "trunc".to_string(), None)
                } else {
                    if !self.is_object_value(a0) {
                        return Err(Thrown(
                            "TypeError: options must be an object or undefined".into(),
                        ));
                    }
                    let fsd = self.read_fsd(a0)?;
                    let mode = self.read_rounding_mode_opt(a0)?;
                    let su = self.read_tostring_unit_token(a0)?;
                    (fsd, mode, su)
                };
                // The `timeZone` option: undefined -> UTC shown as "Z"; otherwise the
                // instant is expressed in that zone and the numeric offset is shown.
                let tz_v = if self.is_object_value(a0) {
                    self.get_prop(a0, "timeZone")?
                } else {
                    Value::UNDEFINED
                };
                let (unit, digits, omit) = Self::tostring_precision(su.as_deref(), fsd)?;
                let (offset, tz_str) = if tz_v == Value::UNDEFINED {
                    (0i64, "Z".to_string())
                } else {
                    // A named zone's offset is the one it has AT this instant.
                    let (id, _) = self.parse_tz_arg(tz_v)?;
                    let off = tz_offset_ns_at(&id, ns);
                    (off, format_offset_rounded(off))
                };
                let rounded = round_increment_as_if_positive(ns, unit, &mode);
                let local = rounded + offset as i128;
                let t = ns_to_time(local.rem_euclid(DAY_NS));
                let (y, mo, d) = epoch_days_to_iso(local.div_euclid(DAY_NS) as i64);
                let s = format!(
                    "{}T{}{}",
                    iso_date_string(y, mo, d),
                    format_time_part(&t, digits, omit),
                    tz_str
                );
                Ok(Some(self.alloc_str(s)))
            }
            "valueOf" => Err(Thrown("TypeError: Called Temporal.Instant.prototype.valueOf".into())),
            "equals" => {
                let o = self.to_instant_ns(a0)?;
                Ok(Some(Value::bool(ns == o)))
            }
            "toZonedDateTimeISO" | "toZonedDateTime" => {
                let (id, offset) = self.parse_tz_arg(a0)?;
                Ok(Some(self.alloc_zdt(ns, offset, id)?))
            }
            "add" | "subtract" => {
                // The EXACT f64 record: a single huge sub-second field (e.g.
                // nanoseconds 1.728e22, spanning min→max) is a valid duration
                // that must not saturate through i64. Integer-valued f64 → i128
                // conversion is exact.
                let dur = self.to_duration_f64(a0)?;
                if dur[0] != 0.0 || dur[1] != 0.0 || dur[2] != 0.0 || dur[3] != 0.0 {
                    return Err(Thrown(
                        "RangeError: Instant arithmetic does not accept calendar (date) units".into(),
                    ));
                }
                let sign: i128 = if name == "add" { 1 } else { -1 };
                let dns = ((dur[4] as i128) * 3_600_000_000_000
                    + (dur[5] as i128) * 60_000_000_000
                    + (dur[6] as i128) * 1_000_000_000
                    + (dur[7] as i128) * 1_000_000
                    + (dur[8] as i128) * 1_000
                    + (dur[9] as i128))
                    * sign;
                Ok(Some(self.make_instant(ns + dns)?))
            }
            "until" | "since" => {
                let o = self.to_instant_ns(a0)?;
                let a1 = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let (largest, smallest, inc, mode) = self.read_time_diff_options(a1, "second")?;
                let diff = if name == "until" { o - ns } else { ns - o };
                let inc_ns = unit_ns(&smallest) * inc;
                let rounded = round_increment(diff, inc_ns, &mode);
                Ok(Some(self.make_duration(balance_duration_ns(rounded, &largest)?)))
            }
            "round" => {
                let (su, inc, mode) = self.read_round_options(
                    a0,
                    &["hour", "minute", "second", "millisecond", "microsecond", "nanosecond"],
                    false,
                )?;
                let inc_ns = unit_ns(&su) * inc;
                // Instant rounding increments must evenly divide a 24-hour day.
                if DAY_NS % inc_ns != 0 {
                    return Err(Thrown(
                        "RangeError: roundingIncrement does not divide evenly into a day".into(),
                    ));
                }
                let rounded = round_increment_as_if_positive(ns, inc_ns, &mode);
                Ok(Some(self.make_instant(rounded)?))
            }
            _ => Ok(None),
        }
    }

    // ── Temporal.PlainYearMonth ──

}
