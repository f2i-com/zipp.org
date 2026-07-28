// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    pub(crate) fn make_plain_year_month(&mut self, y: i64, m: i64, ref_day: i64) -> Result<Value, Thrown> {
        // The reference ISO day (explicit via the 4-arg ctor) must be a valid day
        // of the month (e.g. 32 is a RangeError). The LIMIT is year-month-granular
        // (iso_year_month_in_range) — NOT the day-granular ISODate bound, which
        // would wrongly reject valid boundary year-months (min day<19 / max day>13).
        if !(1..=12).contains(&m)
            || !iso_year_month_in_range(y, m)
            || ref_day < 1
            || ref_day > days_in_month(y, m)
        {
            return Err(Thrown("RangeError: invalid year-month value".into()));
        }
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 5, fields: vec![y, m, ref_day] });
        if self.plainyearmonth_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.plainyearmonth_proto));
        }
        Ok(Value::heap(idx))
    }

    /// CreateTemporalYearMonth in a calendar: the stored ISO date is the FIRST
    /// day of the CALENDAR month, so projecting it back reads the same
    /// year/month. (A non-ISO calendar month straddles two ISO months, which is
    /// exactly why the reference day cannot just be 1.)
    pub(crate) fn make_plain_year_month_cal(
        &mut self,
        cal: Cal,
        cy: i64,
        cm: i64,
        reject: bool,
    ) -> Result<Value, Thrown> {
        let (iy, im, id) = cal_date_to_iso(cal, cy, cm, 1, reject)
            .ok_or_else(|| Thrown("RangeError: invalid year-month value".into()))?;
        let r = self.make_plain_year_month(iy, im, id)?;
        Ok(self.tag_cal(r, cal))
    }

    pub(crate) fn pym_fields(&self, idx: u32) -> Option<(i64, i64, i64)> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 5, fields } => {
                Some((fields[0], fields[1], *fields.get(2).unwrap_or(&1)))
            }
            _ => None,
        }
    }

    pub(crate) fn to_plain_year_month(&mut self, v: Value) -> Result<(i64, i64, i64), Thrown> {
        self.to_plain_year_month_overflow(v, None).map(|(t, _)| t)
    }

    /// ToTemporalYearMonth, returning the stored ISO reference date and the
    /// calendar it belongs to.
    pub(crate) fn to_plain_year_month_overflow(
        &mut self,
        v: Value,
        options: Option<Value>,
    ) -> Result<((i64, i64, i64), Cal), Thrown> {
        if v.is_heap() {
            if let Some(t) = self.pym_fields(v.heap_index()) {
                let cal = self.cal_of(v.heap_index());
                if let Some(o) = options {
                    self.read_overflow(o)?;
                }
                return Ok((t, cal));
            }
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                if !temporal_string_ok(&s, true, true) {
                    return Err(Thrown(format!("RangeError: invalid year-month string '{s}'")));
                }
                // A DATELESS year-month string ("1976-11") cannot carry a non-ISO
                // calendar: with no day there is no way to place it in a calendar
                // whose months straddle the ISO ones. A full date ("2024-06-08
                // [u-ca=islamicc]") can.
                let cal = self.calendar_from_annotation(&s)?;
                if cal != Cal::Iso && !temporal_string_has_date(&s) {
                    return Err(Thrown(format!(
                        "RangeError: year-month string '{s}' must use the ISO 8601 calendar"
                    )));
                }
                let (y, m, d) = parse_iso_year_month(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid year-month string '{s}'")))?;
                if !iso_year_month_in_range(y, m) {
                    return Err(Thrown(format!(
                        "RangeError: year-month '{s}' is outside the representable range"
                    )));
                }
                // ISO yearMonthFromFields sets [[ISODay]] = 1: the day parsed from the
                // string is validated above but dropped (the 4-arg constructor's
                // explicit referenceISODay is a separate path and keeps its value).
                // A non-ISO calendar instead anchors on ITS month's first day.
                if cal == Cal::Iso {
                    return Ok(((y, m, 1), cal));
                }
                let (cy, cm, _) = cal_from_iso(cal, y, m, d);
                let iso = cal_date_to_iso(cal, cy, cm, 1, false)
                    .ok_or_else(|| Thrown("RangeError: invalid year-month value".into()))?;
                return Ok((iso, cal));
            }
            if self.is_object_value(v) {
                let cal = self.validate_iso_calendar_field(v)?;
                // Alphabetical field order: era, eraYear, month, monthCode, year. A
                // calendar-invalid monthCode's RangeError defers past required-field
                // presence (TypeError) and the year coercion (Symbol -> TypeError).
                let (era, era_year) = self.read_era_fields(v, cal)?;
                let m_raw = self.read_month_field_raw(v, cal)?;
                let yv = self.get_prop(v, "year")?;
                if (yv == Value::UNDEFINED && era.is_none() && era_year.is_none())
                    || m_raw.is_none()
                {
                    return Err(Thrown(
                        "TypeError: PlainYearMonth-like requires year and month".into(),
                    ));
                }
                if era.is_some() != era_year.is_some() {
                    return Err(Thrown("TypeError: era and eraYear must be given together".into()));
                }
                // ToIntegerWithTruncation: reject a non-finite (NaN/±Infinity) year
                // and run the observable ToPrimitive (not the non-`&mut` to_number).
                let y_opt = if yv == Value::UNDEFINED {
                    None
                } else {
                    Some(self.temporal_ctor_int(yv)?)
                };
                let (m_val, m_valid, m_conflict) = m_raw.unwrap();
                // GetTemporalOverflowOption: read + validate options.overflow AFTER the
                // field GETs + year coercion (order-of-operations) but BEFORE the
                // calendar-invalid monthCode RangeError; absent options → constrain.
                let reject = if let Some(o) = options {
                    self.read_overflow(o)?
                } else {
                    false
                };
                let y = Self::resolve_cal_year(cal, era.as_deref(), era_year, y_opt)?;
                if !m_valid {
                    return Err(Thrown(format!(
                        "RangeError: monthCode is not valid for the {} calendar",
                        cal.id()
                    )));
                }
                if !cal_month_fields_agree(cal, y, m_val, m_conflict) {
                    return Err(Thrown("RangeError: month and monthCode must agree".into()));
                }
                // month < 1 always rejects; the upper bound is constrained/rejected
                // by CalendarDateToISO below.
                if m_val.floor() < 1 {
                    return Err(Thrown("RangeError: month out of range".into()));
                }
                let m_ord = m_val
                    .ordinal(cal, y, reject)
                    .ok_or_else(|| Thrown("RangeError: month out of range".into()))?;
                let (iy, im, id) = cal_date_to_iso(cal, y, m_ord, 1, reject)
                    .ok_or_else(|| Thrown("RangeError: month out of range".into()))?;
                if !iso_year_month_in_range(iy, im) {
                    return Err(Thrown(
                        "RangeError: year-month is outside the representable range".into(),
                    ));
                }
                return Ok(((iy, im, id), cal));
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.PlainYearMonth".into()))
    }

    /// Read the `month`/`monthCode` fields, returning
    /// `(month, calendar_valid, other)`. `calendar_valid` is false only for a
    /// *well-formed* monthCode that this calendar NEVER has (a leap month it does
    /// not define, or an ordinal past its month count) — a numeric `month` is
    /// always reported valid (its upper bound is constrained/rejected later; its
    /// lower bound is a field-prep floor enforced by the caller). `other` carries
    /// the numeric `month` when a monthCode was ALSO given, so the caller can run
    /// the agreement check once it has resolved the calendar year (the two only
    /// line up per-year in a leap-month calendar). Malformed monthCode SYNTAX
    /// still throws eagerly; the rest is deferred so the caller can raise the
    /// required-field TypeErrors and read its options bag first, per
    /// CalendarResolveFields.
    pub(crate) fn read_month_field_raw(
        &mut self,
        obj: Value,
        cal: Cal,
    ) -> Result<Option<(MonthRef, bool, Option<i64>)>, Thrown> {
        // Read both `month` and `monthCode` (alphabetical field order puts `month`
        // first). When both are present they must agree.
        let month_opt = self.opt_int_field(obj, "month")?;
        let mc = self.get_prop(obj, "monthCode")?;
        if mc != Value::UNDEFINED {
            // monthCode is converted with ToPrimitive(string) then RequireString:
            // an object whose `toString`/`@@toPrimitive` yields a string is fine, but
            // a value that resolves to a non-string (number/bigint/boolean/symbol) is
            // a TypeError. Malformed syntax is a (field-prep) RangeError below.
            let prim = self.to_primitive_string(mc)?;
            if !(prim.is_heap() && self.heap.is_str_like(prim.heap_index())) {
                return Err(Thrown("TypeError: monthCode must be a string".into()));
            }
            let s = self.heap.str_cow(prim.heap_index()).unwrap().into_owned();
            let (code_month, is_leap) = parse_month_code_syntax(&s)
                .ok_or_else(|| Thrown(format!("RangeError: invalid monthCode '{s}'")))?;
            let calendar_valid = cal_month_code_valid(cal, code_month, is_leap);
            return Ok(Some((MonthRef::Code(code_month, is_leap), calendar_valid, month_opt)));
        }
        Ok(month_opt.map(|m| (MonthRef::Ordinal(m), true, None)))
    }

    /// Read the `era`/`eraYear` fields. Calendars without eras (only `iso8601`
    /// here) do not list them in their field set, so they are not even Get —
    /// `{ era: "foobar", year: 1970, … }` must be accepted, era simply ignored.
    pub(crate) fn read_era_fields(
        &mut self,
        obj: Value,
        cal: Cal,
    ) -> Result<(Option<String>, Option<i64>), Thrown> {
        if !cal.has_eras() {
            return Ok((None, None));
        }
        let ev = self.get_prop(obj, "era")?;
        let era = if ev == Value::UNDEFINED {
            None
        } else {
            // Like monthCode: ToPrimitive(string) then RequireString.
            let prim = self.to_primitive_string(ev)?;
            if !(prim.is_heap() && self.heap.is_str_like(prim.heap_index())) {
                return Err(Thrown("TypeError: era must be a string".into()));
            }
            Some(self.heap.str_cow(prim.heap_index()).unwrap().into_owned())
        };
        let era_year = self.opt_int_field(obj, "eraYear")?;
        Ok((era, era_year))
    }

    /// CalendarResolveFields' year step. An era/eraYear PAIR wins over `year`
    /// (they are mutually exclusive inputs, and NonIsoFieldKeysToIgnore drops
    /// whichever the caller did not supply); an unknown era code is a RangeError.
    pub(crate) fn resolve_cal_year(
        cal: Cal,
        era: Option<&str>,
        era_year: Option<i64>,
        year: Option<i64>,
    ) -> Result<i64, Thrown> {
        if let (Some(e), Some(ey)) = (era, era_year) {
            let resolved = cal_resolve_era(cal, &e.to_ascii_lowercase(), ey).ok_or_else(|| {
                Thrown(format!("RangeError: \"{e}\" is not an era of the {} calendar", cal.id()))
            })?;
            // Supplying `year` as well is allowed only when it agrees (the era
            // pair is otherwise the authority — out-of-bounds era years are
            // remapped rather than rejected).
            if year.is_some_and(|y| y != resolved) {
                return Err(Thrown(
                    "RangeError: year does not agree with era and eraYear".into(),
                ));
            }
            return Ok(resolved);
        }
        year.ok_or_else(|| Thrown("TypeError: a year (or era and eraYear) is required".into()))
    }

    /// The calendar named by the FIRST `[u-ca=…]` annotation of a Temporal ISO
    /// string (later ones are ignored); no annotation means iso8601. The string
    /// has already passed `temporal_string_ok`, so an unknown id cannot reach here.
    pub(crate) fn calendar_from_annotation(&self, s: &str) -> Result<Cal, Thrown> {
        let Some(p) = s.find("u-ca=") else { return Ok(Cal::Iso) };
        let val = &s[p + 5..];
        let end = val.find(']').unwrap_or(val.len());
        calendar_by_id(&val[..end])
            .ok_or_else(|| Thrown(format!("RangeError: unsupported calendar \"{}\"", &val[..end])))
    }

    /// Read + validate a property-bag `calendar` field, returning the calendar it
    /// selects (absent → iso8601). Accepts a bare identifier, an embedded
    /// `[u-ca=…]` annotation or a bare ISO string; anything else is a RangeError.
    pub(crate) fn validate_iso_calendar_field(&mut self, obj: Value) -> Result<Cal, Thrown> {
        let cv = self.get_prop(obj, "calendar")?;
        self.validate_calendar_value(cv)
    }

    /// Validate a Temporal calendar VALUE — a positional constructor calendar arg or
    /// a property-bag `calendar` field — and resolve it to a calendar. `undefined`
    /// (→ iso8601) or a calendar-bearing Temporal instance (→ its calendar) is
    /// accepted; a wrong type (null/boolean/number/bigint/symbol/non-calendar
    /// object) is a TypeError, an unknown / unimplemented / malformed calendar
    /// string a RangeError.
    pub(crate) fn validate_calendar_value(&mut self, cv: Value) -> Result<Cal, Thrown> {
        if cv == Value::UNDEFINED {
            return Ok(Cal::Iso);
        }
        if cv.is_heap() {
            // A Temporal instance that carries a calendar (Date/DateTime/YearMonth/
            // MonthDay/ZonedDateTime) is accepted; Duration/PlainTime/Instant have
            // no calendar, so they (and any plain object) are a TypeError.
            if let HeapObj::Temporal { kind, .. } = self.heap.get(cv.heap_index()) {
                return if matches!(kind, 1 | 3 | 5 | 6 | 7) {
                    Ok(self.cal_of(cv.heap_index()))
                } else {
                    Err(Thrown("TypeError: value is not a valid calendar".into()))
                };
            }
            if self.heap.is_str_like(cv.heap_index()) {
                let s = self.heap.str_cow(cv.heap_index()).unwrap().into_owned();
                return match calendar_id_from_string(&s) {
                    Some(id) => calendar_by_id(&id).ok_or_else(|| {
                        Thrown(format!("RangeError: unsupported calendar \"{id}\""))
                    }),
                    None => Err(Thrown(format!("RangeError: invalid calendar \"{s}\""))),
                };
            }
            // A non-string, non-Temporal object (incl. Symbol/BigInt) is invalid.
            return Err(Thrown("TypeError: value is not a valid calendar".into()));
        }
        // A non-string primitive (null/boolean/number) is a TypeError, not a
        // bad calendar string.
        Err(Thrown("TypeError: value is not a valid calendar".into()))
    }

    /// A Temporal *constructor*'s calendar argument must be a bare calendar
    /// IDENTIFIER — NOT a full ISO date / annotated string. (Those are only
    /// accepted by `withCalendar` and the property-bag `calendar` field, which
    /// keep using `validate_calendar_value`.) Non-string cases are identical to
    /// the general validator.
    pub(crate) fn validate_calendar_identifier(&mut self, cv: Value) -> Result<Cal, Thrown> {
        if cv.is_heap() && self.heap.is_str_like(cv.heap_index()) {
            let s = self.heap.str_cow(cv.heap_index()).unwrap().into_owned();
            return calendar_by_id(s.trim()).ok_or_else(|| {
                Thrown(format!("RangeError: \"{s}\" is not a valid calendar identifier"))
            });
        }
        self.validate_calendar_value(cv)
    }

    /// The calendar-facing date getters, shared by every calendar-bearing
    /// Temporal type: `iso` is the instance's stored ISO date, which the
    /// calendar projects into era/eraYear/year/month/monthCode/day and the
    /// month/year length queries (`CalendarISOToDate`). Returns `None` for a
    /// key this does not own, so the caller falls through to its own getters.
    pub(crate) fn cal_date_getter(
        &mut self,
        cal: Cal,
        iso: (i64, i64, i64),
        key: &str,
    ) -> Option<Value> {
        let (iy, im, id) = iso;
        let (y, m, d) = cal_from_iso(cal, iy, im, id);
        Some(match key {
            "year" => Value::num(y as f64),
            "month" => Value::num(m as f64),
            "day" => Value::num(d as f64),
            "monthCode" => {
                let s = month_code_string(cal, y, m);
                self.alloc_str(s)
            }
            "era" => match cal_era(cal, y, m, d) {
                Some((e, _)) => self.alloc_str(e.to_string()),
                None => Value::UNDEFINED,
            },
            "eraYear" => match cal_era(cal, y, m, d) {
                Some((_, ey)) => Value::num(ey as f64),
                None => Value::UNDEFINED,
            },
            "daysInMonth" => Value::num(cal_days_in_month(cal, y, m) as f64),
            "daysInYear" => Value::num(cal_days_in_year(cal, y) as f64),
            "monthsInYear" => Value::num(cal_months_in_year(cal, y) as f64),
            "inLeapYear" => Value::bool(cal_in_leap_year(cal, y)),
            "dayOfYear" => Value::num(
                (iso_to_epoch_days(iy, im, id) - cal_to_epoch_days(cal, y, 1, 1) + 1) as f64,
            ),
            // ISO-8601 week numbering is defined only for the ISO calendar; every
            // other calendar reports undefined rather than inventing one.
            "weekOfYear" => {
                if cal == Cal::Iso {
                    Value::num(iso_week_of_year(iy, im, id) as f64)
                } else {
                    Value::UNDEFINED
                }
            }
            "yearOfWeek" => {
                if cal == Cal::Iso {
                    Value::num(iso_year_of_week(iy, im, id) as f64)
                } else {
                    Value::UNDEFINED
                }
            }
            "calendarId" => self.alloc_str(cal.id().to_string()),
            _ => return None,
        })
    }

    /// The calendar of a Temporal instance (`iso8601` unless tagged).
    pub(crate) fn cal_of(&self, idx: u32) -> Cal {
        Cal::from_u8(self.temporal_cal.get(&idx).copied().unwrap_or(0))
    }

    /// Tag a freshly built Temporal instance with its calendar and return it.
    /// `iso8601` is the table's absent state, so it costs nothing.
    pub(crate) fn tag_cal(&mut self, v: Value, cal: Cal) -> Value {
        if cal != Cal::Iso && v.is_heap() {
            self.temporal_cal.insert(v.heap_index(), cal as u8);
        }
        v
    }

    pub(crate) fn plain_year_month_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let (y, m, rd) = match self.pym_fields(idx) {
            Some(t) => t,
            None => return Ok(None),
        };
        let cal = self.cal_of(idx);
        // The calendar year/month this instance denotes (its stored ISO date is
        // the first day of that calendar month).
        let (cy, cm, _) = cal_from_iso(cal, y, m, rd);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "toJSON" => {
                let suf = self.calendar_name_suffix(Value::UNDEFINED, cal)?;
                let s = if suf.is_empty() {
                    year_month_string(y, m)
                } else {
                    format!("{}{}", iso_date_string(y, m, rd), suf)
                };
                Ok(Some(self.alloc_str(s)))
            }
            "toString" => {
                // A calendar annotation (always/critical, or any non-ISO calendar)
                // makes the reference ISO day part of the serialization.
                let suf = self.calendar_name_suffix(a0, cal)?;
                let s = if suf.is_empty() {
                    year_month_string(y, m)
                } else {
                    format!("{}{}", iso_date_string(y, m, rd), suf)
                };
                Ok(Some(self.alloc_str(s)))
            }
            "valueOf" => {
                Err(Thrown("TypeError: Called Temporal.PlainYearMonth.prototype.valueOf".into()))
            }
            "equals" => {
                // PlainYearMonth equality includes the reference ISO day and the
                // calendar id.
                let (o, ocal) = self.to_plain_year_month_overflow(a0, None)?;
                Ok(Some(Value::bool((y, m, rd) == (o.0, o.1, o.2) && cal == ocal)))
            }
            "with" => {
                self.reject_temporal_like(a0)?;
                // Field reads (observable getters) happen in alphabetical key order
                // (era, eraYear, month, monthCode, year), all BEFORE the options bag.
                let (era, era_year) = self.read_era_fields(a0, cal)?;
                let mf = self.read_month_field_raw(a0, cal)?;
                let yf = self.opt_int_field(a0, "year")?;
                if yf.is_none() && mf.is_none() && era.is_none() && era_year.is_none() {
                    return Err(Thrown(
                        "TypeError: with() requires at least one recognized property".into(),
                    ));
                }
                if era.is_some() != era_year.is_some() {
                    return Err(Thrown("TypeError: era and eraYear must be given together".into()));
                }
                // NonIsoFieldKeysToIgnore: era+eraYear and year are mutually exclusive.
                let ny = if era.is_some() || yf.is_some() {
                    Self::resolve_cal_year(cal, era.as_deref(), era_year, yf)?
                } else {
                    cy
                };
                let month_valid = mf.map(|(_, v, _)| v).unwrap_or(true);
                let month_conflict = mf.and_then(|(_, _, c)| c);
                // A bag with no month keeps the receiver's MONTH CODE, not its
                // ordinal: `hebrew.with({ year })` from a leap year into a common
                // one must stay on the same named month.
                let nm = mf.map(|(mm, _, _)| mm).unwrap_or(MonthRef::of(cal, cy, cm));
                // month uses ToPositiveIntegerWithTruncation: a value below 1 is rejected
                // during field preparation, BEFORE the options bag is read.
                if nm.floor() < 1 {
                    return Err(Thrown("RangeError: invalid date fields".into()));
                }
                let reject = self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
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
                let nm = nm
                    .ordinal(cal, ny, reject)
                    .ok_or_else(|| Thrown("RangeError: invalid date fields".into()))?;
                Ok(Some(self.make_plain_year_month_cal(cal, ny, nm, reject)?))
            }
            "add" | "subtract" => {
                let dur = self.to_duration(a0)?;
                // The overflow option is still validated (constrain/reject/RangeError
                // on bad values) and read before the algorithmic range check below.
                let reject = self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                let sign = if name == "add" { 1 } else { -1 };
                let op_sign = sign * Self::duration_sign(&dur);
                // A PlainYearMonth carries no day, so any unit smaller than a month
                // (weeks/days/time) is unrepresentable: ToDateDurationRecordWithoutTime
                // rejects a nonzero sub-month remainder → RangeError, regardless of
                // sign, overflow option, or whether the result would land in range.
                // (This subsumes the old end-of-month overflow check for the maximum.)
                if dur[2..].iter().any(|&x| x != 0) {
                    return Err(Thrown(
                        "RangeError: PlainYearMonth.prototype.add/subtract does not accept units smaller than months".into(),
                    ));
                }
                // The intermediate ISO date (Day = 1) must still be within the
                // day-granular ISO limits, so every op on the minimum -271821-04
                // (whose Day 1 = -271821-04-01 < the min ISO date -271821-04-19) throws.
                if !iso_date_in_range(y, m, 1) {
                    return Err(Thrown(
                        "RangeError: PlainYearMonth is outside the valid ISO date range".into(),
                    ));
                }
                // Reference day per spec: start of month for non-negative ops, end of
                // month for negative — so day/week units don't spill into a wrong month.
                let ref_day = if op_sign < 0 { cal_days_in_month(cal, cy, cm) } else { 1 };
                let anchor = cal_date_to_iso(cal, cy, cm, ref_day, false)
                    .ok_or_else(|| Thrown("RangeError: invalid year-month value".into()))?;
                // A PlainYearMonth has no day, so `overflow: reject` can only bite on
                // the MONTH — a leap month the destination year does not have (hebrew
                // Adar I + 1 year, add/leap-months-hebrew.js). Probed with day 1 so the
                // day clamp, which is not an overflow here, cannot fire.
                if reject
                    && cal_add_year_month(cal, cy, cm, 1, dur[0] * sign, dur[1] * sign, true)
                        .is_none()
                {
                    return Err(Thrown(
                        "RangeError: the month does not exist in the resulting year".into(),
                    ));
                }
                let (ay, am, ad) = self.date_add(cal, anchor.0, anchor.1, anchor.2, &dur, sign);
                let (ry, rm, _) = cal_from_iso(cal, ay, am, ad);
                Ok(Some(self.make_plain_year_month_cal(cal, ry, rm, false)?))
            }
            "until" | "since" => {
                let (o, ocal) = self.to_plain_year_month_overflow(a0, None)?;
                if ocal != cal {
                    return Err(Thrown(
                        "RangeError: cannot compute a difference between year-months in different calendars"
                            .into(),
                    ));
                }
                let opts = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if opts != Value::UNDEFINED && !self.is_object_value(opts) {
                    return Err(Thrown("TypeError: options must be an object or undefined".into()));
                }
                let ym_units = &["auto", "year", "years", "month", "months"];
                // GetDifferenceSettings order: read+cast largestUnit, roundingIncrement,
                // roundingMode, smallestUnit before validating. smallestUnit default
                // "month"; largestUnit default "auto" → "year".
                let largest_str = self.opt_string_raw(opts, "largestUnit", "auto")?;
                let inc = self.read_rounding_increment(opts)?;
                let mode = self.read_rounding_mode(opts, "trunc")?;
                let smallest_str = self.opt_string_raw(opts, "smallestUnit", "month")?;
                self.unit_allowed(&largest_str, "largestUnit", ym_units)?;
                self.unit_allowed(&smallest_str, "smallestUnit", ym_units)?;
                let smallest = normalize_unit(&smallest_str, "month");
                let largest_raw = normalize_unit(&largest_str, "auto");
                let largest = if largest_raw == "auto" { "year".to_string() } else { largest_raw };
                let rank = |u: &str| if u == "year" { 0 } else { 1 };
                if rank(&smallest) < rank(&largest) {
                    return Err(Thrown(
                        "RangeError: smallestUnit is larger than largestUnit".into(),
                    ));
                }
                // Difference in CALENDAR months, via the global month index so that
                // years of 12 and 13 months (hebrew) both count for their real length.
                let miy = cal_months_in_year(cal, cy);
                let (oy, om, _) = cal_from_iso(cal, o.0, o.1, o.2);
                let from = cal_month_index(cal, cy, cm);
                let to = cal_month_index(cal, oy, om);
                let total_months = if name == "until" { to - from } else { from - to };
                // DifferenceTemporalPlainYearMonth sets each operand's reference Day=1
                // and range-checks that date; the very-minimum YM (-271821-04) has a
                // day-1 reference date (-271821-04-01) BELOW the ISO date limit, so a
                // non-zero difference involving it is a RangeError. Equal operands
                // short-circuit to a zero Duration before any range check.
                if total_months != 0
                    && (!iso_date_in_range(y, m, 1) || !iso_date_in_range(o.0, o.1, 1))
                {
                    return Err(Thrown(
                        "RangeError: PlainYearMonth difference is outside the representable range".into(),
                    ));
                }
                // NudgeToCalendarUnit: when the difference is actually rounded, the
                // away-from-zero candidate end (start + r2 in the smallest unit, as a
                // day-1 DATE per CalendarDateAdd) must lie within the ISO date range,
                // regardless of which way rounding resolves. No rounding happens for
                // smallestUnit "month" with increment 1, and equal operands return a
                // zero Duration before any nudge.
                if (inc != 1 || smallest != "month") && total_months != 0 {
                    // Spec orientation: the nudged span is receiver → other.
                    let d_months = if name == "until" { total_months } else { -total_months };
                    let s = if d_months < 0 { -1i64 } else { 1 };
                    let inc_i = inc as i64;
                    let cand_idx = if smallest == "year" {
                        let r2y = (d_months / miy / inc_i + s) * inc_i;
                        cal_month_index(cal, cy + r2y, cm)
                    } else if largest == "year" {
                        // Years split out first; only the months remainder is rounded.
                        let years = d_months / miy;
                        let r2m = (d_months % miy / inc_i + s) * inc_i;
                        from + years * miy + r2m
                    } else {
                        let r2m = (d_months / inc_i + s) * inc_i;
                        from + r2m
                    };
                    let (ky, km) = cal_month_from_index(cal, cand_idx);
                    let kiso = cal_date_to_iso(cal, ky, km, 1, false);
                    if kiso.is_none_or(|(a, b, c)| !iso_date_in_range(a, b, c)) {
                        return Err(Thrown(
                            "RangeError: rounded PlainYearMonth difference is outside the representable range"
                                .into(),
                        ));
                    }
                }
                let mut f = [0i64; 10];
                if largest == "year" {
                    // Split the years out by MONTH CODE, exactly as the date
                    // difference does — `total / monthsInYear` would be wrong in a
                    // calendar whose years hold 12 or 13 months, and even in a
                    // 12-month Hebrew year 12 months need not make a year
                    // (until/leap-months-hebrew.js).
                    // Always split the RECEIVER → other direction and negate for
                    // `since`; swapping the operands would anchor the year step on the
                    // wrong date, and the split is deliberately not antisymmetric.
                    let (years, ay, am, span) =
                        cal_until_year_split(cal, (cy, cm, 1), (oy, om, 1));
                    let months = cal_month_index(cal, oy, om) - cal_month_index(cal, ay, am);
                    let (years, months) =
                        if name == "until" { (years, months) } else { (-years, -months) };
                    if smallest == "year" {
                        // Round years + (months / span), scaled so `span` months are
                        // one year; `span` is the real length of the year the leftover
                        // months are eating into.
                        f[0] = round_increment(
                            (years * span + months) as i128,
                            span as i128 * inc,
                            &mode,
                        ) as i64
                            / span;
                    } else {
                        // Years aren't rounded; only the months remainder is. It can
                        // carry into a year only if ROUNDING grew it — the exact
                        // remainder never does, however close to `span` it is.
                        let rm = round_increment(months as i128, inc, &mode) as i64;
                        let carry = if rm.abs() > months.abs() { rm / span } else { 0 };
                        f[0] = years + carry;
                        f[1] = rm - carry * span;
                    }
                } else {
                    f[1] = round_increment(total_months as i128, inc, &mode) as i64;
                }
                Ok(Some(self.make_duration(f.map(|x| x as f64))))
            }
            "toPlainDate" => {
                let day = self.opt_int_field(a0, "day")?.ok_or_else(|| {
                    Thrown("TypeError: toPlainDate requires a day".into())
                })?;
                // Default overflow is "constrain": clamp the day to the month.
                let (iy, im, id) = cal_date_to_iso(cal, cy, cm, day, false)
                    .ok_or_else(|| Thrown("RangeError: invalid date fields".into()))?;
                let r = self.make_plain_date(iy, im, id)?;
                Ok(Some(self.tag_cal(r, cal)))
            }
            "getISOFields" => {
                let cal = self.alloc_str(cal.id().to_string());
                let mut o = ObjMap::new();
                o.set("isoYear", Value::num(y as f64));
                o.set("isoMonth", Value::num(m as f64));
                o.set("isoDay", Value::num(rd as f64));
                o.set("calendar", cal);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Object(Box::new(o))))))
            }
            _ => Ok(None),
        }
    }

    // ── Temporal.PlainMonthDay ──

    pub(crate) fn make_plain_month_day(&mut self, m: i64, d: i64, ref_year: i64) -> Result<Value, Thrown> {
        // CreateTemporalMonthDay runs ISODateWithinLimits on the FULL reference
        // date (day-granular is correct here: an explicit referenceISOYear at the
        // boundary admits only the in-range days, e.g. +275760-09-13 but not -14).
        if !(1..=12).contains(&m)
            || d < 1
            || d > days_in_month(ref_year, m)
            || !iso_date_in_range(ref_year, m, d)
        {
            return Err(Thrown("RangeError: invalid month-day value".into()));
        }
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 6, fields: vec![ref_year, m, d] });
        if self.plainmonthday_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.plainmonthday_proto));
        }
        Ok(Value::heap(idx))
    }

    /// CalendarMonthDayToISOReferenceDate: the reference ISO date of a
    /// calendar month/day is the LATEST one not after 1972-12-31 that has those
    /// calendar fields — so a leap-only day (Coptic M13/6, ISO 02-29) still gets
    /// a real anchor. Walks back at most a leap cycle.
    pub(crate) fn make_plain_month_day_fields(
        &mut self,
        cal: Cal,
        code: (i64, bool),
        cd: i64,
    ) -> Result<Value, Thrown> {
        if cal == Cal::Iso {
            return self.make_plain_month_day(code.0, cd, 1972);
        }
        let limit = iso_to_epoch_days(1972, 12, 31);
        let mut cy = cal_from_epoch_days(cal, limit).0;
        for _ in 0..40 {
            // A leap-month calendar skips years that do not HAVE this month at
            // all: hebrew "M05L" anchors on the last leap year before 1973.
            if let Some(cm) = cal_month_of_code(cal, cy, code.0, code.1) {
                if cd <= cal_days_in_month(cal, cy, cm) {
                    let ed = cal_to_epoch_days(cal, cy, cm, cd);
                    if ed <= limit {
                        let (iy, im, id) = epoch_days_to_iso(ed);
                        let r = self.make_plain_month_day(im, id, iy)?;
                        return Ok(self.tag_cal(r, cal));
                    }
                }
            }
            cy -= 1;
        }
        Err(Thrown("RangeError: month-day is not valid in this calendar".into()))
    }

    /// CreateTemporalMonthDay from an ISO date reinterpreted in `cal`.
    pub(crate) fn make_plain_month_day_cal(
        &mut self,
        cal: Cal,
        iso: (i64, i64, i64),
    ) -> Result<Value, Thrown> {
        let (cy, cm, cd) = cal_from_iso(cal, iso.0, iso.1, iso.2);
        self.make_plain_month_day_fields(cal, cal_month_code(cal, cy, cm), cd)
    }

    pub(crate) fn pmd_fields(&self, idx: u32) -> Option<(i64, i64, i64)> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 6, fields } => {
                Some((fields[0], fields[1], fields[2]))
            }
            _ => None,
        }
    }

    pub(crate) fn to_plain_month_day(&mut self, v: Value) -> Result<(i64, i64, i64), Thrown> {
        self.to_plain_month_day_overflow(v, None).map(|(t, _)| t)
    }

    pub(crate) fn to_plain_month_day_overflow(
        &mut self,
        v: Value,
        options: Option<Value>,
    ) -> Result<((i64, i64, i64), Cal), Thrown> {
        if v.is_heap() {
            if let Some(t) = self.pmd_fields(v.heap_index()) {
                let cal = self.cal_of(v.heap_index());
                if let Some(o) = options {
                    self.read_overflow(o)?;
                }
                return Ok((t, cal));
            }
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                if !temporal_string_ok(&s, true, true) {
                    return Err(Thrown(format!("RangeError: invalid month-day string '{s}'")));
                }
                let cal = self.calendar_from_annotation(&s)?;
                if cal != Cal::Iso {
                    // A non-ISO calendar needs a FULL date to project ("11-18"
                    // names no year), and that date must be representable — the
                    // ISO path can ignore an out-of-range year because it keeps
                    // only the month/day, but a projection cannot.
                    let iso = temporal_string_date(&s).filter(|&(y, m, d)| iso_date_in_range(y, m, d));
                    let iso = iso.ok_or_else(|| {
                        Thrown(format!("RangeError: invalid month-day string '{s}'"))
                    })?;
                    let v = self.make_plain_month_day_cal(cal, iso)?;
                    let t = self.pmd_fields(v.heap_index()).unwrap();
                    return Ok((t, cal));
                }
                let t = parse_iso_month_day(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid month-day string '{s}'")))?;
                return Ok((t, cal));
            }
            if self.is_object_value(v) {
                let cal = self.validate_iso_calendar_field(v)?;
                // Alphabetical field order: day, era, eraYear, month, monthCode, year.
                let d_opt = self.opt_int_field(v, "day")?;
                let (era, era_year) = self.read_era_fields(v, cal)?;
                let m_raw = self.read_month_field_raw(v, cal)?;
                // The reference `year` field is read (and finite-checked, rejecting
                // ±Infinity/NaN) even though the ISO calendar always stores 1972 as
                // the reference ISO year. Required-field presence + this year coercion
                // precede a calendar-invalid monthCode's RangeError.
                let year_field = self.opt_int_field(v, "year")?;
                if m_raw.is_none() || d_opt.is_none() {
                    return Err(Thrown(
                        "TypeError: PlainMonthDay-like requires month and day".into(),
                    ));
                }
                if era.is_some() != era_year.is_some() {
                    return Err(Thrown("TypeError: era and eraYear must be given together".into()));
                }
                let (m_val, m_valid, m_conflict) = m_raw.unwrap();
                // In a non-ISO calendar an ORDINAL month names nothing on its own —
                // which month it is depends on the year (a leap-month calendar can
                // even shift it) — so `month` without `year` is a TypeError, not a
                // RangeError (PlainMonthDay/from/fields-object.js).
                let numeric_month = matches!(m_val, MonthRef::Ordinal(_)) || m_conflict.is_some();
                if cal != Cal::Iso && numeric_month && year_field.is_none() && era.is_none() {
                    return Err(Thrown(
                        "TypeError: a non-ISO PlainMonthDay needs monthCode, or month with year"
                            .into(),
                    ));
                }
                // GetTemporalOverflowOption: read + validate options.overflow AFTER the
                // field GETs + year coercion (order-of-operations) but BEFORE the
                // calendar-invalid monthCode RangeError; absent options → constrain.
                let reject = if let Some(o) = options {
                    self.read_overflow(o)?
                } else {
                    false
                };
                if !m_valid {
                    return Err(Thrown(format!(
                        "RangeError: monthCode is not valid for the {} calendar",
                        cal.id()
                    )));
                }
                let mut m = m_val;
                let mut d = d_opt.unwrap();
                if m.floor() < 1 || d < 1 {
                    return Err(Thrown("RangeError: month-day out of range".into()));
                }
                if cal != Cal::Iso {
                    // A supplied year (or era pair) must name a real, representable
                    // date in this calendar. Only iso8601 gets the special case
                    // where the year is consulted for overflow but never
                    // range-checked (built-ins .../from/iso-year-used-only-for-overflow).
                    let stated =
                        if era.is_some() || year_field.is_some() {
                            Some(Self::resolve_cal_year(cal, era.as_deref(), era_year, year_field)?)
                        } else {
                            None
                        };
                    if let Some(y) = stated {
                        if !cal_month_fields_agree(cal, y, m, m_conflict) {
                            return Err(Thrown(
                                "RangeError: month and monthCode must agree".into(),
                            ));
                        }
                        let iso = m
                            .ordinal(cal, y, reject)
                            .and_then(|mo| cal_date_to_iso(cal, y, mo, d, reject))
                            .filter(|&(iy, im, id)| iso_date_in_range(iy, im, id));
                        if iso.is_none() {
                            return Err(Thrown("RangeError: month-day out of range".into()));
                        }
                        // With a year in hand the ordinal is meaningful; carry the
                        // month on as the CODE it names there, so the reference-year
                        // search below looks for the right month.
                        let mo = m.ordinal(cal, y, false).unwrap();
                        m = MonthRef::of(cal, y, mo);
                    }
                    // Anchor on the calendar's own reference year; the day is
                    // regulated against that month's length there.
                    let code = match m {
                        MonthRef::Code(n, l) => (n, l),
                        MonthRef::Ordinal(n) => {
                            // Unreachable for non-ISO (a bare ordinal was rejected
                            // above), but keep the bound rather than panicking.
                            let last = cal_max_months(cal);
                            if reject && !(1..=last).contains(&n) {
                                return Err(Thrown("RangeError: month-day out of range".into()));
                            }
                            (n.min(last), false)
                        }
                    };
                    let anchor = self.make_plain_month_day_fields(cal, code, d);
                    let v = match anchor {
                        Ok(v) => v,
                        Err(e) => {
                            if reject {
                                return Err(e);
                            }
                            // constrain: clamp to the LONGEST this month ever is —
                            // the reference date is free to be a leap year, so
                            // Coptic M13 day 7 constrains to 6, not to 5.
                            let cy = cal_from_epoch_days(cal, iso_to_epoch_days(1972, 12, 31)).0;
                            let cd = d.min(cal_month_code_max_days(cal, cy, code.0, code.1));
                            self.make_plain_month_day_fields(cal, code, cd)?
                        }
                    };
                    let t = self.pmd_fields(v.heap_index()).unwrap();
                    return Ok((t, cal));
                }
                // iso8601: month and monthCode are interchangeable (no leap months),
                // so the agreement check needs no year.
                if !cal_month_fields_agree(cal, 1972, m, m_conflict) {
                    return Err(Thrown("RangeError: month and monthCode must agree".into()));
                }
                let mut m = m.ordinal(cal, 1972, false).unwrap();
                // The supplied `year` (if any) decides whether the day overflows
                // (e.g. Feb-29 is valid only in a leap year); absent → 1972 (leap),
                // so a bare {month:2,day:29} stays valid. The stored reference year
                // is always 1972 regardless.
                let eff_year = year_field.unwrap_or(1972);
                if reject {
                    if !(1..=12).contains(&m) || d > days_in_month(eff_year, m) {
                        return Err(Thrown("RangeError: month-day out of range".into()));
                    }
                } else {
                    // "constrain" clamps only the upper bound.
                    m = m.min(12);
                    d = d.min(days_in_month(eff_year, m));
                }
                return Ok(((1972, m, d), cal));
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.PlainMonthDay".into()))
    }

    pub(crate) fn plain_month_day_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
        let (ry, m, d) = match self.pmd_fields(idx) {
            Some(t) => t,
            None => return Ok(None),
        };
        let cal = self.cal_of(idx);
        let (ccy, ccm, ccd) = cal_from_iso(cal, ry, m, d);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "toJSON" => {
                let suf = self.calendar_name_suffix(Value::UNDEFINED, cal)?;
                let s = if suf.is_empty() {
                    format!("{m:02}-{d:02}")
                } else {
                    format!("{}{}", iso_date_string(ry, m, d), suf)
                };
                Ok(Some(self.alloc_str(s)))
            }
            "toString" => {
                // A calendar annotation (always/critical, or any non-ISO calendar)
                // makes the reference ISO year part of the serialization.
                let suf = self.calendar_name_suffix(a0, cal)?;
                let s = if suf.is_empty() {
                    format!("{m:02}-{d:02}")
                } else {
                    format!("{}{}", iso_date_string(ry, m, d), suf)
                };
                Ok(Some(self.alloc_str(s)))
            }
            "valueOf" => {
                Err(Thrown("TypeError: Called Temporal.PlainMonthDay.prototype.valueOf".into()))
            }
            "equals" => {
                let (o, ocal) = self.to_plain_month_day_overflow(a0, None)?;
                Ok(Some(Value::bool((ry, m, d) == o && cal == ocal)))
            }
            "with" => {
                self.reject_temporal_like(a0)?;
                // Field reads (observable getters) happen in alphabetical key order
                // (day, month, monthCode, year), all BEFORE reading the options bag.
                let df = self.opt_int_field(a0, "day")?;
                let mf = self.read_month_field_raw(a0, cal)?;
                let yf = self.opt_int_field(a0, "year")?;
                if yf.is_none() && mf.is_none() && df.is_none() {
                    return Err(Thrown(
                        "TypeError: with() requires at least one recognized property".into(),
                    ));
                }
                let month_valid = mf.map(|(_, v, _)| v).unwrap_or(true);
                let month_conflict = mf.and_then(|(_, _, c)| c);
                // No month field keeps the receiver's — as a CODE for a non-ISO
                // calendar, since its reference year is re-chosen below.
                let nm_ref = mf.map(|(mm, _, _)| mm).unwrap_or(if cal == Cal::Iso {
                    MonthRef::Ordinal(m)
                } else {
                    MonthRef::of(cal, ccy, ccm)
                });
                let mut nd = df.unwrap_or(if cal == Cal::Iso { d } else { ccd });
                // month/day use ToPositiveIntegerWithTruncation: a value below 1 is
                // rejected during field preparation, BEFORE the options bag is read.
                if nm_ref.floor() < 1 || nd < 1 {
                    return Err(Thrown("RangeError: invalid date fields".into()));
                }
                let reject = self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                // A month/monthCode conflict, or a well-formed-but-calendar-invalid
                // monthCode ("M08L", "M13"), is rejected only after the options bag.
                if !month_valid {
                    return Err(Thrown(format!(
                        "RangeError: monthCode is not valid for the {} calendar",
                        cal.id()
                    )));
                }
                // `with` has no year field to resolve against, so the agreement check
                // uses the receiver's own calendar year.
                if !cal_month_fields_agree(cal, ccy, nm_ref, month_conflict) {
                    return Err(Thrown("RangeError: month and monthCode must agree".into()));
                }
                if cal != Cal::Iso {
                    let code = match nm_ref {
                        MonthRef::Code(n, l) => (n, l),
                        MonthRef::Ordinal(n) => {
                            let last = cal_max_months(cal);
                            if reject && !(1..=last).contains(&n) {
                                return Err(Thrown("RangeError: month-day out of range".into()));
                            }
                            cal_month_code(cal, ccy, n.min(last))
                        }
                    };
                    return Ok(Some(self.make_plain_month_day_fields(cal, code, nd)?));
                }
                let mut nm = nm_ref.ordinal(cal, ry, false).unwrap();
                // The (optional) `year` field is used ONLY to apply the overflow option
                // to the day (e.g. whether Feb 29 fits) — it is never range-checked. The
                // result keeps the instance's canonical ISO reference year.
                let eff_year = yf.unwrap_or(ry);
                if reject {
                    if !(1..=12).contains(&nm) || nd > days_in_month(eff_year, nm) {
                        return Err(Thrown("RangeError: month-day out of range".into()));
                    }
                } else {
                    // "constrain" clamps only the upper bound.
                    nm = nm.min(12);
                    nd = nd.min(days_in_month(eff_year, nm));
                }
                Ok(Some(self.make_plain_month_day(nm, nd, ry)?))
            }
            "toPlainDate" => {
                let year = self.opt_int_field(a0, "year")?.ok_or_else(|| {
                    Thrown("TypeError: toPlainDate requires a year".into())
                })?;
                // Default overflow is "constrain": clamp the day to the month (e.g.
                // PlainMonthDay(2,29).toPlainDate({year:2023}) → 2023-02-28). The
                // month travels as a CODE — the reference year's ordinal need not be
                // the target year's (hebrew M05L is ordinal 6 only in a leap year).
                let (num, leap) = cal_month_code(cal, ccy, ccm);
                let mo = MonthRef::Code(num, leap).ordinal(cal, year, false).unwrap();
                let (iy, im, id) = cal_date_to_iso(cal, year, mo, ccd, false)
                    .ok_or_else(|| Thrown("RangeError: invalid date fields".into()))?;
                let r = self.make_plain_date(iy, im, id)?;
                Ok(Some(self.tag_cal(r, cal)))
            }
            "getISOFields" => {
                let cal = self.alloc_str(cal.id().to_string());
                let mut o = ObjMap::new();
                o.set("isoYear", Value::num(ry as f64));
                o.set("isoMonth", Value::num(m as f64));
                o.set("isoDay", Value::num(d as f64));
                o.set("calendar", cal);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Object(Box::new(o))))))
            }
            _ => Ok(None),
        }
    }

    // ── Intl ──

}
