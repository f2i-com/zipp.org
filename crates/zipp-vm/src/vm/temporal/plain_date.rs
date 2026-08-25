// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    pub(crate) fn make_plain_date(&mut self, y: i64, m: i64, d: i64) -> Result<Value, Thrown> {
        if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) || !iso_date_in_range(y, m, d)
        {
            return Err(Thrown("RangeError: invalid ISO date".into()));
        }
        let idx = self.heap.alloc(HeapObj::Temporal {
            kind: 1,
            fields: vec![y, m, d],
        });
        if self.plaindate_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.plaindate_proto));
        }
        Ok(Value::heap(idx))
    }

    pub(crate) fn plain_date_fields(&self, idx: u32) -> Option<(i64, i64, i64)> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 1, fields } => Some((fields[0], fields[1], fields[2])),
            _ => None,
        }
    }

    /// ToTemporalDate: a PlainDate clones; a string parses; an object reads year/
    /// month/day (PlainDateTime also has these — accepted).
    /// Read the `overflow` option: false = "constrain" (the default), true =
    /// "reject". Any other value is a RangeError.
    pub(crate) fn read_overflow(&mut self, options: Value) -> Result<bool, Thrown> {
        if options == Value::UNDEFINED {
            return Ok(false);
        }
        if !self.is_object_value(options) {
            return Err(Thrown(
                "TypeError: options must be an object or undefined".into(),
            ));
        }
        let v = self.get_prop(options, "overflow")?;
        if v == Value::UNDEFINED {
            return Ok(false);
        }
        match self.to_js_string(v)?.as_str() {
            "constrain" => Ok(false),
            "reject" => Ok(true),
            other => Err(Thrown(format!(
                "RangeError: invalid overflow value: {other}"
            ))),
        }
    }

    /// Validate the ZonedDateTime resolution options (disambiguation / offset /
    /// overflow) for their throwing side effects (a bad value or wrong type is a
    /// RangeError; a non-object, non-undefined `options` is a TypeError). The
    /// single-offset model can't act on disambiguation/offset, but their values
    /// must still be in range. Returns the overflow `reject` flag.
    /// Read a ZonedDateTime resolution options bag, returning `(offset option,
    /// overflow-is-reject)`. The offset option (default "reject") governs how a bag/
    /// string `offset` that disagrees with the time zone is resolved.
    pub(crate) fn read_zdt_options(
        &mut self,
        options: Value,
        offset_default: &str,
    ) -> Result<(String, String, bool), Thrown> {
        if options != Value::UNDEFINED && !self.is_object_value(options) {
            return Err(Thrown(
                "TypeError: options must be an object or undefined".into(),
            ));
        }
        let disamb = self.opt_string(
            options,
            "disambiguation",
            "compatible",
            &["compatible", "earlier", "later", "reject"],
        )?;
        // `from` defaults the offset option to "reject"; `with` defaults to "prefer".
        let off = self.opt_string(
            options,
            "offset",
            offset_default,
            &["prefer", "use", "ignore", "reject"],
        )?;
        let reject = self.read_overflow(options)?;
        Ok((off, disamb, reject))
    }

    /// Resolve a toString() options bag (fractionalSecondDigits / smallestUnit /
    /// roundingMode) into (round-unit ns, fractional digits [-1=auto, 0..9],
    /// omit-seconds, roundingMode). smallestUnit wins over fractionalSecondDigits.
    pub(crate) fn time_precision(
        &mut self,
        options: Value,
    ) -> Result<(i128, i32, bool, String), Thrown> {
        if options == Value::UNDEFINED {
            return Ok((1, -1, false, "trunc".to_string()));
        }
        if !self.is_object_value(options) {
            return Err(Thrown(
                "TypeError: options must be an object or undefined".into(),
            ));
        }
        // ToSecondsStringPrecision reads options in spec order — fractionalSecondDigits,
        // then roundingMode, then smallestUnit — casting each (the observable
        // get/.toString sequence the order-of-operations tests assert) before applying
        // precedence. smallestUnit, when present, wins over fractionalSecondDigits, but
        // fsd's read+cast+validation still occurs first. smallestUnit is the LAST
        // option here, so class-validating right after its read is observably correct;
        // ZDT/Instant toString read more options in between and compose the pieces.
        let fsd = self.read_fsd(options)?;
        let mode = self.read_rounding_mode_opt(options)?;
        let su = self.read_tostring_unit_token(options)?;
        let (unit, digits, omit) = Self::tostring_precision(su.as_deref(), fsd)?;
        Ok((unit, digits, omit, mode))
    }

    /// GetTemporalFractionalSecondDigitsOption: `auto`/undefined → (1ns, -1,
    /// trim-zeros); 0..9 → fixed digits. Caller guarantees an object bag.
    pub(crate) fn read_fsd(&mut self, options: Value) -> Result<(i128, i32, bool), Thrown> {
        let fsd_v = self.get_prop(options, "fractionalSecondDigits")?;
        if fsd_v == Value::UNDEFINED {
            Ok((1, -1, false))
        } else if !fsd_v.is_number() {
            // A string/null/boolean/bigint/object is ToString'd and must be "auto"
            // (a Symbol throws TypeError inside to_js_string).
            if self.to_js_string(fsd_v)? == "auto" {
                Ok((1, -1, false))
            } else {
                Err(Thrown(
                    "RangeError: fractionalSecondDigits must be 'auto' or 0..9".into(),
                ))
            }
        } else {
            // A genuine Number is floored into 0..9 (GetStringOrNumberOption).
            let n = self.to_number(fsd_v)?;
            if n.is_nan() {
                return Err(Thrown("RangeError: fractionalSecondDigits is NaN".into()));
            }
            let n = n.floor() as i64;
            if !(0..=9).contains(&n) {
                return Err(Thrown(
                    "RangeError: fractionalSecondDigits out of range".into(),
                ));
            }
            Ok((10i128.pow(9 - n as u32), n as i32, false))
        }
    }

    /// options.roundingMode for the toString paths (default "trunc").
    pub(crate) fn read_rounding_mode_opt(&mut self, options: Value) -> Result<String, Thrown> {
        self.opt_string(
            options,
            "roundingMode",
            "trunc",
            &[
                "ceil",
                "floor",
                "trunc",
                "expand",
                "halfCeil",
                "halfFloor",
                "halfTrunc",
                "halfEven",
                "halfExpand",
            ],
        )
    }

    /// Read options.smallestUnit as a unit TOKEN: any real Temporal unit is
    /// accepted here (garbage strings RangeError at read); whether the unit
    /// CLASS is allowed for a time-style toString is deferred to
    /// [`Self::tostring_precision`], after any later options are read (the
    /// options-read-before-algorithmic-validation ordering).
    pub(crate) fn read_tostring_unit_token(
        &mut self,
        options: Value,
    ) -> Result<Option<String>, Thrown> {
        let su_v = self.get_prop(options, "smallestUnit")?;
        if su_v == Value::UNDEFINED {
            return Ok(None);
        }
        let su = normalize_unit(&self.to_js_string(su_v)?, "");
        if !matches!(
            su.as_str(),
            "year"
                | "month"
                | "week"
                | "day"
                | "hour"
                | "minute"
                | "second"
                | "millisecond"
                | "microsecond"
                | "nanosecond"
        ) {
            return Err(Thrown(format!(
                "RangeError: invalid smallestUnit for toString: {su}"
            )));
        }
        Ok(Some(su))
    }

    /// ToSecondsStringPrecision unit-class application: minute..nanosecond are
    /// valid toString smallest units (minute omits seconds); a date unit or
    /// "hour" is a RangeError. None falls back to fractionalSecondDigits.
    pub(crate) fn tostring_precision(
        su: Option<&str>,
        fsd: (i128, i32, bool),
    ) -> Result<(i128, i32, bool), Thrown> {
        match su {
            None => Ok(fsd),
            Some("minute") => Ok((60_000_000_000, 0, true)),
            Some("second") => Ok((1_000_000_000, 0, false)),
            Some("millisecond") => Ok((1_000_000, 3, false)),
            Some("microsecond") => Ok((1_000, 6, false)),
            Some("nanosecond") => Ok((1, 9, false)),
            Some(u) => Err(Thrown(format!(
                "RangeError: invalid smallestUnit for toString: {u}"
            ))),
        }
    }

    /// The calendar annotation suffix for a toString() per the `calendarName`
    /// option: "always" → "[u-ca=<id>]", "critical" → "[!u-ca=<id>]", "never" →
    /// "". "auto" (the default) emits the annotation for every calendar EXCEPT
    /// iso8601, which is the default calendar and stays implicit.
    ///
    /// The second result is the EXPANDED flag TemporalYearMonthToString /
    /// TemporalMonthDayToString need: "If showCalendar is always or critical, OR
    /// calendarIdentifier is not iso8601, add the reference day/year". That
    /// condition is NOT the same as "an annotation is printed" — with
    /// `{calendarName: "never"}` on a non-ISO calendar the annotation is dropped
    /// but the reference component stays (`toString/calendarname-never.js`).
    pub(crate) fn calendar_name_suffix_expanded(
        &mut self,
        options: Value,
        cal: Cal,
    ) -> Result<(String, bool), Thrown> {
        let id = cal.id();
        let non_iso = cal != Cal::Iso;
        if options == Value::UNDEFINED {
            let suf = if non_iso {
                format!("[u-ca={id}]")
            } else {
                String::new()
            };
            return Ok((suf, non_iso));
        }
        if !self.is_object_value(options) {
            return Err(Thrown(
                "TypeError: options must be an object or undefined".into(),
            ));
        }
        let cn = self.opt_string(
            options,
            "calendarName",
            "auto",
            &["auto", "always", "never", "critical"],
        )?;
        let suf = match cn.as_str() {
            "always" => format!("[u-ca={id}]"),
            "critical" => format!("[!u-ca={id}]"),
            "auto" if non_iso => format!("[u-ca={id}]"),
            _ => String::new(),
        };
        let expanded = non_iso || matches!(cn.as_str(), "always" | "critical");
        Ok((suf, expanded))
    }

    /// `calendar_name_suffix_expanded` for the types whose serialization never
    /// varies in shape (PlainDate/PlainDateTime/ZonedDateTime always print the
    /// full ISO date).
    pub(crate) fn calendar_name_suffix(
        &mut self,
        options: Value,
        cal: Cal,
    ) -> Result<String, Thrown> {
        self.calendar_name_suffix_expanded(options, cal)
            .map(|(s, _)| s)
    }

    pub(crate) fn to_plain_date(&mut self, v: Value) -> Result<(i64, i64, i64), Thrown> {
        self.to_plain_date_overflow(v, None)
    }

    pub(crate) fn to_plain_date_overflow(
        &mut self,
        v: Value,
        options: Option<Value>,
    ) -> Result<(i64, i64, i64), Thrown> {
        self.to_plain_date_cal(v, options).map(|(d, _)| d)
    }

    /// ToTemporalDate with an overflow mode (constrain clamps; reject throws on
    /// out-of-range fields), returning the ISO date AND the calendar it came
    /// with — a PlainDate/PlainDateTime/ZonedDateTime carries its own, a bag
    /// takes its `calendar` field, and a string its `[u-ca=…]` annotation.
    pub(crate) fn to_plain_date_cal(
        &mut self,
        v: Value,
        options: Option<Value>,
    ) -> Result<((i64, i64, i64), Cal), Thrown> {
        if v.is_heap() {
            if let Some(t) = self.plain_date_fields(v.heap_index()) {
                let cal = self.cal_of(v.heap_index());
                if let Some(o) = options {
                    self.read_overflow(o)?;
                }
                return Ok((t, cal));
            }
            // A ZonedDateTime or PlainDateTime yields its calendar date.
            if let HeapObj::Temporal { kind, .. } = self.heap.get(v.heap_index()) {
                let date = match kind {
                    7 => Some(self.zdt_local(v.heap_index())),
                    3 => self.pdt_fields(v.heap_index()),
                    _ => None,
                };
                if let Some(f) = date {
                    let cal = self.cal_of(v.heap_index());
                    if let Some(o) = options {
                        self.read_overflow(o)?;
                    }
                    return Ok(((f[0], f[1], f[2]), cal));
                }
            }
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                if !temporal_string_ok(&s, true, true) {
                    return Err(Thrown(format!("RangeError: invalid date string '{s}'")));
                }
                let cal = self.calendar_from_annotation(&s)?;
                let (y, m, d) = parse_iso_date(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid date string '{s}'")))?;
                if !iso_date_in_range(y, m, d) {
                    return Err(Thrown(format!(
                        "RangeError: date '{s}' is outside the representable range"
                    )));
                }
                return Ok(((y, m, d), cal));
            }
            if self.is_object_value(v) {
                let cal = self.validate_iso_calendar_field(v)?;
                // PrepareCalendarFields reads the fields in ALPHABETICAL order:
                // day, era, eraYear, month, monthCode, year — observable via the
                // property-bag getters' side effects (order-of-operations).
                let d_opt = self.opt_int_field(v, "day")?;
                let (era, era_year) = self.read_era_fields(v, cal)?;
                let m_raw = self.read_month_field_raw(v, cal)?;
                let y_opt = self.opt_int_field(v, "year")?;
                // CalendarResolveFields validates field PRESENCE (TypeError) before
                // any range/consistency check (RangeError), so a bag missing `year`
                // reports the missing field even when its month/monthCode conflict.
                if (y_opt.is_none() && era.is_none() && era_year.is_none())
                    || m_raw.is_none()
                    || d_opt.is_none()
                {
                    return Err(Thrown(
                        "TypeError: PlainDate-like requires year, month, day".into(),
                    ));
                }
                if era.is_some() != era_year.is_some() {
                    return Err(Thrown(
                        "TypeError: era and eraYear must be given together".into(),
                    ));
                }
                // GetTemporalOverflowOption: read + validate options.overflow AFTER
                // the field GETs (order-of-operations) but BEFORE the algorithmic
                // validation (era/monthCode validity, range); absent → constrain.
                let reject = if let Some(o) = options {
                    self.read_overflow(o)?
                } else {
                    false
                };
                let y = Self::resolve_cal_year(cal, era.as_deref(), era_year, y_opt)?;
                let (m_val, m_valid, m_conflict) = m_raw.unwrap();
                if !m_valid {
                    return Err(Thrown(format!(
                        "RangeError: monthCode is not valid for the {} calendar",
                        cal.id()
                    )));
                }
                if !cal_month_fields_agree(cal, y, m_val, m_conflict) {
                    return Err(Thrown("RangeError: month and monthCode must agree".into()));
                }
                let d = d_opt.unwrap();
                // A month/day below 1 is a hard floor that always rejects
                // (RegulateISODate is reached only after the fields are >= 1).
                if m_val.floor() < 1 || d < 1 {
                    return Err(Thrown("RangeError: invalid date fields".into()));
                }
                let (iy, im, id) = m_val
                    .ordinal(cal, y, reject)
                    .and_then(|m| cal_date_to_iso(cal, y, m, d, reject))
                    .ok_or_else(|| Thrown("RangeError: invalid date fields".into()))?;
                if !iso_date_in_range(iy, im, id) {
                    return Err(Thrown(
                        "RangeError: date is outside the representable range".into(),
                    ));
                }
                return Ok(((iy, im, id), cal));
            }
        }
        Err(Thrown(
            "TypeError: cannot convert value to a Temporal.PlainDate".into(),
        ))
    }

    /// Round the date difference from `d1` to `d2` to `smallest` (a calendar unit:
    /// year/month/week), then balance up to `largest`. NudgeToCalendarUnit: the
    /// whole count of the smallest unit, plus the fraction of the way to the next
    /// (measured in days against the anchor calendar), rounded per `mode`. Assumes
    /// roundingIncrement 1 (the spec disallows >1 for calendar units).
    pub(crate) fn round_relative_date_diff(
        &self,
        cal: Cal,
        d1: (i64, i64, i64),
        d2: (i64, i64, i64),
        smallest: &str,
        largest: &str,
        inc: i128,
        mode: &str,
    ) -> Result<[i64; 4], Thrown> {
        let rank = |u: &str| {
            ["year", "month", "week", "day"]
                .iter()
                .position(|&x| x == u)
                .unwrap_or(3)
        };
        let si = rank(smallest);
        let e1 = iso_to_epoch_days(d1.0, d1.1, d1.2);
        let e2 = iso_to_epoch_days(d2.0, d2.1, d2.2);
        let sign = (e2 > e1) as i64 - (e2 < e1) as i64;
        if sign == 0 {
            return Ok([0, 0, 0, 0]);
        }
        // Decompose at largestUnit to KEEP the units above smallestUnit; only the
        // smallestUnit component is rounded (the sub-smallest remainder becomes the
        // fraction toward the next increment — NudgeToCalendarUnit).
        let base = cal_difference_date(cal, d1, d2, largest);
        // smallestUnit = week: difference_iso_date dumps the sub-month remainder into
        // DAYS (weeks = 0 when largestUnit > week), so derive the whole-week count from
        // the full sub-week day span instead of the (zeroed) week field.
        let sval = if si == 2 {
            (base[2] * 7 + base[3]) / 7
        } else {
            base[si]
        };
        let mk = |k: i64| -> [i64; 10] {
            let mut dur = [0i64; 10];
            dur[..si].copy_from_slice(&base[..si]);
            dur[si] = k;
            dur
        };
        let mut r1 = round_increment(sval as i128, inc, "trunc") as i64;
        // NudgeToCalendarUnit brackets the target between r1 and r1+increment; the
        // difference under-counts at an end-of-month anchor (see the datetime
        // twin in temporal/mod.rs), so advance while the next increment fits.
        loop {
            let nxt = r1 + inc as i64 * sign;
            let cand = self.date_add(cal, d1.0, d1.1, d1.2, &mk(nxt), 1);
            let e = iso_to_epoch_days(cand.0, cand.1, cand.2);
            if (sign > 0 && e <= e2) || (sign < 0 && e >= e2) {
                r1 = nxt;
            } else {
                break;
            }
        }
        let r2 = r1 + inc as i64 * sign;
        let lower = self.date_add(cal, d1.0, d1.1, d1.2, &mk(r1), 1);
        let ld = iso_to_epoch_days(lower.0, lower.1, lower.2);
        // The r2 endpoint is a CalendarDateAdd(constrain) that must lie within the
        // ISO date limits — a huge increment can push it past the range (RangeError).
        let upper = self.date_add(cal, d1.0, d1.1, d1.2, &mk(r2), 1);
        if !iso_date_in_range(upper.0, upper.1, upper.2) {
            return Err(Thrown(
                "RangeError: rounded date is outside the valid ISO range".into(),
            ));
        }
        let picked = if ld == e2 {
            r1
        } else {
            let ud = iso_to_epoch_days(upper.0, upper.1, upper.2);
            let denom = (ud - ld) as f64;
            let progress = if denom != 0.0 {
                (e2 - ld) as f64 / denom
            } else {
                0.0
            };
            // Round the increment-quotient (r1/inc), preserving its parity for
            // halfEven, then scale back. At inc==1 this is round_fraction(sval, …).
            round_fraction(r1 / inc as i64, sign, progress, mode) * inc as i64
        };
        // Weeks never fold into a larger calendar unit, so keep years/months + the
        // rounded weeks + 0 days. Year/month results re-balance to largestUnit (which
        // folds an overflowing smallest unit up, e.g. 12 months → 1 year).
        if si == 2 {
            return Ok([base[0], base[1], picked, 0]);
        }
        // BubbleRelativeDuration: keep the larger units + the rounded smallest one.
        let d = mk(picked);
        let mut out = [d[0], d[1], d[2], d[3]];
        if si == 1 && largest == "year" {
            let miy = cal_months_in_year(cal, d1.0);
            out[0] += out[1] / miy;
            out[1] %= miy;
        }
        Ok(out)
    }

    /// `date ± duration` (date units constrain day; time units fold to whole days).
    pub(crate) fn date_add(
        &self,
        cal: Cal,
        y: i64,
        m: i64,
        d: i64,
        dur: &[i64; 10],
        sign: i64,
    ) -> (i64, i64, i64) {
        self.date_add_overflow(cal, y, m, d, dur, sign, false)
            .unwrap()
    }

    /// `date ± duration` with an overflow mode. The years/months step happens in
    /// CALENDAR space (that is what makes "one month later" mean a month of the
    /// user's calendar) and can land the day past the new month's length:
    /// "constrain" clamps it, "reject" throws. Weeks/days then add via exact
    /// epoch-day math, which is calendar-independent.
    pub(crate) fn date_add_overflow(
        &self,
        cal: Cal,
        y: i64,
        m: i64,
        d: i64,
        dur: &[i64; 10],
        sign: i64,
        reject: bool,
    ) -> Result<(i64, i64, i64), Thrown> {
        let (cy, cm, cd) = cal_from_iso(cal, y, m, d);
        let (ny, nm, nd) =
            cal_add_year_month(cal, cy, cm, cd, dur[0] * sign, dur[1] * sign, reject)
                .ok_or_else(|| Thrown("RangeError: date arithmetic overflows the month".into()))?;
        let time_ns = (dur[4] as i128) * 3_600_000_000_000
            + (dur[5] as i128) * 60_000_000_000
            + (dur[6] as i128) * 1_000_000_000
            + (dur[7] as i128) * 1_000_000
            + (dur[8] as i128) * 1_000
            + (dur[9] as i128);
        let extra_days = (time_ns / 86_400_000_000_000) as i64;
        let ed = cal_to_epoch_days(cal, ny, nm, nd) + (dur[2] * 7 + dur[3] + extra_days) * sign;
        Ok(epoch_days_to_iso(ed))
    }

    pub(crate) fn plain_date_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let (y, m, d) = match self.plain_date_fields(idx) {
            Some(t) => t,
            None => return Ok(None),
        };
        let cal = self.cal_of(idx);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "toJSON" => {
                // toJSON is toString with default options, so it carries the
                // calendar annotation for every non-ISO calendar.
                let suf = self.calendar_name_suffix(Value::UNDEFINED, cal)?;
                Ok(Some(self.alloc_str(format!(
                    "{}{}",
                    iso_date_string(y, m, d),
                    suf
                ))))
            }
            "toString" => {
                let suf = self.calendar_name_suffix(a0, cal)?;
                Ok(Some(self.alloc_str(format!(
                    "{}{}",
                    iso_date_string(y, m, d),
                    suf
                ))))
            }
            "valueOf" => Err(Thrown(
                "TypeError: Called Temporal.PlainDate.prototype.valueOf".into(),
            )),
            "equals" => {
                // CompareISODate AND the calendar id: two dates on the same day in
                // different calendars are not equal.
                let (other, ocal) = self.to_plain_date_cal(a0, None)?;
                Ok(Some(Value::bool((y, m, d) == other && cal == ocal)))
            }
            // The reference is canonical: the first day of the calendar month for a
            // year-month, and the ISO year 1972 for a month-day.
            "toPlainYearMonth" => {
                let (cy, cm, _) = cal_from_iso(cal, y, m, d);
                let (ry, rm, rd) = cal_date_to_iso(cal, cy, cm, 1, false)
                    .ok_or_else(|| Thrown("RangeError: invalid year-month value".into()))?;
                let r = self.make_plain_year_month(ry, rm, rd)?;
                Ok(Some(self.tag_cal(r, cal)))
            }
            "toPlainMonthDay" => {
                let r = self.make_plain_month_day_cal(cal, (y, m, d))?;
                Ok(Some(r))
            }
            "toPlainDateTime" => {
                // Combine this date with a time (ToTemporalTime; default midnight).
                let t = if a0 == Value::UNDEFINED {
                    [0i64; 6]
                } else {
                    self.to_plain_time(a0)?
                };
                let r = self.make_plain_date_time([y, m, d, t[0], t[1], t[2], t[3], t[4], t[5]])?;
                Ok(Some(self.tag_cal(r, cal)))
            }
            "withCalendar" => {
                // ToTemporalCalendarIdentifier(undefined) is a TypeError — the
                // argument is required (validate_calendar_value allows undefined for
                // an optional field-bag calendar, so guard here, not there).
                if a0 == Value::UNDEFINED {
                    return Err(Thrown(
                        "TypeError: withCalendar requires a calendar argument".into(),
                    ));
                }
                let ncal = self.validate_calendar_value(a0)?;
                let r = self.make_plain_date(y, m, d)?;
                Ok(Some(self.tag_cal(r, ncal)))
            }
            "toZonedDateTime" => {
                // Spec item handling: an Object item (incl. a Proxy bag — gating on
                // HeapObj::Object alone missed those) yields timeZone then plainTime
                // via observable Gets; anything else is itself the time-zone-like.
                let (id, time) = if self.is_object_value(a0) {
                    let tzv = self.get_prop(a0, "timeZone")?;
                    if tzv == Value::UNDEFINED {
                        // No timeZone property: the item itself is the
                        // time-zone-like (a ZonedDateTime carries its zone).
                        (self.parse_tz_arg(a0)?.0, None)
                    } else {
                        let (id, _) = self.parse_tz_arg(tzv)?;
                        let pt = self.get_prop(a0, "plainTime")?;
                        let time = if pt == Value::UNDEFINED {
                            None
                        } else {
                            Some(self.to_plain_time(pt)?)
                        };
                        (id, time)
                    }
                } else {
                    (self.parse_tz_arg(a0)?.0, None)
                };
                // With NO plainTime the result is GetStartOfDay, which is not the
                // same as disambiguating midnight: America/Toronto skipped
                // 1919-03-31T00:00, so its day starts at the 00:30 transition
                // while `{plainTime: new PlainTime()}` disambiguates to 01:00.
                let midnight = (iso_to_epoch_days(y, m, d) as i128) * DAY_NS;
                let ns = match time {
                    None => tz_start_of_day(&id, midnight)?,
                    Some(t) => tz_local_to_instant(&id, midnight + time_to_ns(&t), "compatible")?,
                };
                let r = self.alloc_zdt(ns, tz_offset_ns_at(&id, ns), id)?;
                Ok(Some(self.tag_cal(r, cal)))
            }
            "with" => {
                self.reject_temporal_like(a0)?;
                // Field reads (observable getters) happen in alphabetical key order
                // (day, era, eraYear, month, monthCode, year), all BEFORE the
                // options bag.
                let df = self.opt_int_field(a0, "day")?;
                let (era, era_year) = self.read_era_fields(a0, cal)?;
                let mf = self.read_month_field_raw(a0, cal)?;
                let yf = self.opt_int_field(a0, "year")?;
                if yf.is_none()
                    && mf.is_none()
                    && df.is_none()
                    && era.is_none()
                    && era_year.is_none()
                {
                    return Err(Thrown(
                        "TypeError: with() requires at least one recognized property".into(),
                    ));
                }
                if era.is_some() != era_year.is_some() {
                    return Err(Thrown(
                        "TypeError: era and eraYear must be given together".into(),
                    ));
                }
                let (cy, cm, cd) = cal_from_iso(cal, y, m, d);
                // NonIsoFieldKeysToIgnore: era+eraYear and year are mutually
                // exclusive, so whichever the bag supplies replaces the receiver's.
                let ny = if era.is_some() || yf.is_some() {
                    Self::resolve_cal_year(cal, era.as_deref(), era_year, yf)?
                } else {
                    cy
                };
                let month_valid = mf.map(|(_, v, _)| v).unwrap_or(true);
                let month_conflict = mf.and_then(|(_, _, c)| c);
                // A bag with no month keeps the receiver's MONTH CODE, not its
                // ordinal: `with({ year })` across a Hebrew leap-year boundary must
                // stay on the same named month (with/leap-months-hebrew.js).
                let nm = mf.map(|(mm, _, _)| mm).unwrap_or(MonthRef::of(cal, cy, cm));
                let nd = df.unwrap_or(cd);
                // month/day use ToPositiveIntegerWithTruncation: a value below 1 is
                // rejected during field preparation, BEFORE the options bag is read.
                if nm.floor() < 1 || nd < 1 {
                    return Err(Thrown("RangeError: invalid date fields".into()));
                }
                let reject =
                    self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
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
                let (iy, im, id) = nm
                    .ordinal(cal, ny, reject)
                    .and_then(|m| cal_date_to_iso(cal, ny, m, nd, reject))
                    .ok_or_else(|| Thrown("RangeError: invalid date fields".into()))?;
                let r = self.make_plain_date(iy, im, id)?;
                Ok(Some(self.tag_cal(r, cal)))
            }
            "add" | "subtract" => {
                let dur = self.to_duration(a0)?;
                let reject =
                    self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                let sign = if name == "add" { 1 } else { -1 };
                let (ny, nm, nd) = self.date_add_overflow(cal, y, m, d, &dur, sign, reject)?;
                let r = self.make_plain_date(ny, nm, nd)?;
                Ok(Some(self.tag_cal(r, cal)))
            }
            "until" | "since" => {
                let (other, ocal) = self.to_plain_date_cal(a0, None)?;
                if ocal != cal {
                    return Err(Thrown(
                        "RangeError: cannot compute a difference between dates in different calendars"
                            .into(),
                    ));
                }
                let opts = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if opts != Value::UNDEFINED && !self.is_object_value(opts) {
                    return Err(Thrown(
                        "TypeError: options must be an object or undefined".into(),
                    ));
                }
                let date_units = &[
                    "auto", "year", "years", "month", "months", "week", "weeks", "day", "days",
                ];
                // GetDifferenceSettings order-of-operations: read (and cast) all
                // options first — largestUnit, roundingIncrement, roundingMode,
                // smallestUnit — and only THEN validate. smallestUnit defaults to
                // "day", largestUnit to "auto" (→ the larger of smallestUnit/"day").
                let largest_str = self.opt_string_raw(opts, "largestUnit", "auto")?;
                let inc = self.read_rounding_increment(opts)?;
                let mode = self.read_rounding_mode(opts, "trunc")?;
                let smallest_str = self.opt_string_raw(opts, "smallestUnit", "day")?;
                self.unit_allowed(&largest_str, "largestUnit", date_units)?;
                self.unit_allowed(&smallest_str, "smallestUnit", date_units)?;
                let smallest = normalize_unit(&smallest_str, "day");
                let largest_raw = normalize_unit(&largest_str, "auto");
                let order = ["year", "month", "week", "day"];
                let rank = |u: &str| order.iter().position(|&x| x == u).unwrap_or(3);
                let largest = if largest_raw == "auto" {
                    if rank(&smallest) < 3 {
                        smallest.clone()
                    } else {
                        "day".to_string()
                    }
                } else {
                    largest_raw
                };
                if rank(&smallest) < rank(&largest) {
                    return Err(Thrown(
                        "RangeError: smallestUnit is larger than largestUnit".into(),
                    ));
                }
                // since = negate(until): always compute the forward (this → other)
                // difference with a sign-negated rounding mode, then negate the result.
                // (Swapping operands for `since` would anchor the day-of-month borrow on
                // the wrong date.)
                let (d1, d2) = ((y, m, d), other);
                let eff = if name == "since" {
                    negate_mode(&mode)
                } else {
                    mode.clone()
                };
                let mut f = [0i64; 10];
                // The day field rounds to the increment; a calendar smallestUnit
                // (year/month/week) rounds the fractional remainder against the
                // anchor calendar (NudgeToCalendarUnit) and balances to largestUnit.
                let si = rank(&smallest);
                if si == 3 {
                    let diff = cal_difference_date(cal, d1, d2, &largest);
                    f[..4].copy_from_slice(&diff);
                    f[3] = round_increment(f[3] as i128, inc, &eff) as i64;
                } else {
                    let r =
                        self.round_relative_date_diff(cal, d1, d2, &smallest, &largest, inc, &eff)?;
                    f[..4].copy_from_slice(&r);
                }
                if name == "since" {
                    f.iter_mut().for_each(|x| *x = -*x);
                }
                Ok(Some(self.make_duration(f.map(|x| x as f64))))
            }
            "getISOFields" => {
                let cal = self.alloc_str(cal.id().to_string());
                let mut o = ObjMap::new();
                o.set("isoYear", Value::num(y as f64));
                o.set("isoMonth", Value::num(m as f64));
                o.set("isoDay", Value::num(d as f64));
                o.set("calendar", cal);
                Ok(Some(Value::heap(
                    self.heap.alloc(HeapObj::Object(Box::new(o))),
                )))
            }
            _ => Ok(None),
        }
    }

    /// Read the `roundingIncrement` option: ToNumber (valueOf-aware; a BigInt is a
    /// TypeError, like the spec's ToNumber), then require a finite integer >= 1.
    pub(crate) fn read_rounding_increment(&mut self, opts: Value) -> Result<i128, Thrown> {
        if opts == Value::UNDEFINED {
            return Ok(1);
        }
        let v = self.get_prop(opts, "roundingIncrement")?;
        if v == Value::UNDEFINED {
            return Ok(1);
        }
        if v.is_heap()
            && matches!(
                self.heap.get(v.heap_index()),
                HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
            )
        {
            return Err(Thrown(
                "TypeError: Cannot convert a BigInt value to a number".into(),
            ));
        }
        // ToIntegerWithTruncation: a non-integer is truncated toward zero (2.5 -> 2),
        // not rejected; only non-finite or < 1 is out of range.
        let n = self.to_number_coerce(v)?;
        if !n.is_finite() {
            return Err(Thrown("RangeError: roundingIncrement out of range".into()));
        }
        // ToTemporalRoundingIncrement: truncate(increment) must be in [1, 10^9].
        let n = n.trunc();
        if n < 1.0 || n > 1_000_000_000.0 {
            return Err(Thrown("RangeError: roundingIncrement out of range".into()));
        }
        Ok(n as i128)
    }

    /// Read the `roundingMode` option, validated against the nine Temporal modes.
    pub(crate) fn read_rounding_mode(
        &mut self,
        opts: Value,
        default: &str,
    ) -> Result<String, Thrown> {
        self.opt_string(
            opts,
            "roundingMode",
            default,
            &[
                "ceil",
                "floor",
                "trunc",
                "expand",
                "halfCeil",
                "halfFloor",
                "halfTrunc",
                "halfEven",
                "halfExpand",
            ],
        )
    }

    /// Read an optional integer field from an options/with object (None if absent).
    /// ToIntegerWithTruncation for a Temporal *constructor* field: ToNumber, reject a
    /// non-finite value (NaN / ±Infinity) with a RangeError — the spec rejects these
    /// before any component range check — then truncate toward zero. (The property-bag
    /// `from({...})` path uses `opt_int_field`, which already finite-checks.)
    pub(crate) fn temporal_ctor_int(&mut self, v: Value) -> Result<i64, Thrown> {
        // `to_number_strict` runs the observable ToPrimitive (valueOf/@@toPrimitive)
        // on an object field in spec order, then — like ToNumber — rejects a Symbol OR
        // a BigInt with a TypeError (the lenient `to_number`/`to_number_coerce` coerce
        // a BigInt for relational compares, which is wrong for a constructor field).
        let n = self.to_number_strict(v)?;
        if !n.is_finite() {
            return Err(Thrown(
                "RangeError: Temporal field must be a finite number".into(),
            ));
        }
        Ok(n.trunc() as i64)
    }

    /// Guard a `.with()` argument: it must be a plain property bag — not a primitive,
    /// not a Temporal object (the brand check runs BEFORE any property Get, so a
    /// throwing calendar/timeZone getter on a Temporal arg is never invoked), and not
    /// an object carrying a `calendar` or `timeZone` property.
    pub(crate) fn reject_temporal_like(&mut self, arg: Value) -> Result<(), Thrown> {
        if !self.is_object_value(arg) {
            return Err(Thrown(
                "TypeError: with() requires a property-bag object".into(),
            ));
        }
        if arg.is_heap() {
            if let HeapObj::Temporal { .. } = self.heap.get(arg.heap_index()) {
                return Err(Thrown(
                    "TypeError: with() does not accept a Temporal object".into(),
                ));
            }
        }
        if self.get_prop(arg, "calendar")? != Value::UNDEFINED
            || self.get_prop(arg, "timeZone")? != Value::UNDEFINED
        {
            return Err(Thrown(
                "TypeError: with() argument must not have a calendar or timeZone property".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn opt_int_field(&mut self, obj: Value, key: &str) -> Result<Option<i64>, Thrown> {
        let v = self.get_prop(obj, key)?;
        if v == Value::UNDEFINED {
            return Ok(None);
        }
        // ToNumber honours a user valueOf/toString (ToPrimitive) on objects; a
        // non-finite field (Infinity/NaN) is rejected per the spec.
        let n = self.to_number_coerce(v)?;
        if !n.is_finite() {
            return Err(Thrown(format!(
                "RangeError: {key} property must be a finite number"
            )));
        }
        Ok(Some(n.trunc() as i64))
    }

    // ── Temporal.PlainTime ──
}
