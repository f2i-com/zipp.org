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

    pub(crate) fn pym_fields(&self, idx: u32) -> Option<(i64, i64, i64)> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 5, fields } => {
                Some((fields[0], fields[1], *fields.get(2).unwrap_or(&1)))
            }
            _ => None,
        }
    }

    pub(crate) fn to_plain_year_month(&mut self, v: Value) -> Result<(i64, i64, i64), Thrown> {
        self.to_plain_year_month_overflow(v, None)
    }

    pub(crate) fn to_plain_year_month_overflow(
        &mut self,
        v: Value,
        options: Option<Value>,
    ) -> Result<(i64, i64, i64), Thrown> {
        if v.is_heap() {
            if let Some(t) = self.pym_fields(v.heap_index()) {
                if let Some(o) = options {
                    self.read_overflow(o)?;
                }
                return Ok(t);
            }
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                if !temporal_string_ok(&s, true, true) {
                    return Err(Thrown(format!("RangeError: invalid year-month string '{s}'")));
                }
                let (y, m, _) = parse_iso_year_month(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid year-month string '{s}'")))?;
                if !iso_year_month_in_range(y, m) {
                    return Err(Thrown(format!(
                        "RangeError: year-month '{s}' is outside the representable range"
                    )));
                }
                // ISO yearMonthFromFields sets [[ISODay]] = 1: the day parsed from the
                // string is validated above but dropped (the 4-arg constructor's
                // explicit referenceISODay is a separate path and keeps its value).
                return Ok((y, m, 1));
            }
            if self.is_object_value(v) {
                self.validate_iso_calendar_field(v)?;
                // Alphabetical field order: month, monthCode (raw), year. A
                // calendar-invalid monthCode's RangeError defers past required-field
                // presence (TypeError) and the year coercion (Symbol -> TypeError).
                let m_raw = self.read_month_field_raw(v)?;
                let yv = self.get_prop(v, "year")?;
                if yv == Value::UNDEFINED || m_raw.is_none() {
                    return Err(Thrown(
                        "TypeError: PlainYearMonth-like requires year and month".into(),
                    ));
                }
                // ToIntegerWithTruncation: reject a non-finite (NaN/±Infinity) year
                // and run the observable ToPrimitive (not the non-`&mut` to_number).
                let y = self.temporal_ctor_int(yv)?;
                let (m_val, m_valid) = m_raw.unwrap();
                // GetTemporalOverflowOption: read + validate options.overflow AFTER the
                // field GETs + year coercion (order-of-operations) but BEFORE the
                // calendar-invalid monthCode RangeError; absent options → constrain.
                let reject = if let Some(o) = options {
                    self.read_overflow(o)?
                } else {
                    false
                };
                if !m_valid {
                    return Err(Thrown(
                        "RangeError: monthCode is not valid for the ISO 8601 calendar".into(),
                    ));
                }
                let mut m = m_val;
                if reject {
                    if !(1..=12).contains(&m) {
                        return Err(Thrown("RangeError: month out of range".into()));
                    }
                } else {
                    // "constrain" clamps only the upper bound; month < 1 always rejects.
                    if m < 1 {
                        return Err(Thrown("RangeError: month out of range".into()));
                    }
                    m = m.min(12);
                }
                if !iso_year_month_in_range(y, m) {
                    return Err(Thrown(
                        "RangeError: year-month is outside the representable range".into(),
                    ));
                }
                return Ok((y, m, 1));
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.PlainYearMonth".into()))
    }

    /// Read month from an object: monthCode ("M06") takes precedence over `month`.
    pub(crate) fn read_month_field(&mut self, obj: Value) -> Result<Option<i64>, Thrown> {
        // Eager form for the non-`with` paths (from/construction): a calendar-invalid
        // monthCode is rejected immediately, matching the historical behaviour.
        match self.read_month_field_raw(obj)? {
            Some((m, true)) => Ok(Some(m)),
            Some((_, false)) => {
                Err(Thrown("RangeError: monthCode is not valid for the ISO 8601 calendar".into()))
            }
            None => Ok(None),
        }
    }

    /// Read the `month`/`monthCode` fields, returning `(month, calendar_valid)`.
    /// `calendar_valid` is false only for a *well-formed* monthCode that is invalid
    /// for ISO (a leap month, or a month outside 1..=12) — a numeric `month` is always
    /// reported valid (its upper bound is constrained/rejected later; its lower bound
    /// is a field-prep floor enforced by the caller). Malformed monthCode syntax and a
    /// month/monthCode disagreement still throw eagerly here (field-prep errors). The
    /// `with()` handlers defer the `calendar_valid == false` rejection until after the
    /// options bag is read, per spec; [[read_month_field]] rejects it immediately.
    pub(crate) fn read_month_field_raw(
        &mut self,
        obj: Value,
    ) -> Result<Option<(i64, bool)>, Thrown> {
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
            if let Some(m) = month_opt {
                if m != code_month {
                    return Err(Thrown("RangeError: month and monthCode must agree".into()));
                }
            }
            let calendar_valid = !is_leap && (1..=12).contains(&code_month);
            return Ok(Some((code_month, calendar_valid)));
        }
        Ok(month_opt.map(|m| (m, true)))
    }

    /// Validate a property-bag `calendar` field for the ISO-only engine: absent,
    /// or resolving to "iso8601" (case-insensitive, or via an embedded `[u-ca=…]`
    /// annotation / bare ISO string), is accepted; anything else is a RangeError.
    pub(crate) fn validate_iso_calendar_field(&mut self, obj: Value) -> Result<(), Thrown> {
        let cv = self.get_prop(obj, "calendar")?;
        self.validate_calendar_value(cv)
    }

    /// Validate a Temporal calendar VALUE — a positional constructor calendar arg or
    /// a property-bag `calendar` field. `undefined` (→ default iso8601), a
    /// calendar-bearing Temporal instance, or a string resolving to "iso8601" is
    /// accepted; a wrong type (null/boolean/number/bigint/symbol/non-calendar object)
    /// is a TypeError, an unknown / empty / malformed calendar string a RangeError.
    pub(crate) fn validate_calendar_value(&mut self, cv: Value) -> Result<(), Thrown> {
        if cv == Value::UNDEFINED {
            return Ok(());
        }
        if cv.is_heap() {
            // A Temporal instance that carries a calendar (Date/DateTime/YearMonth/
            // MonthDay/ZonedDateTime) is accepted; Duration/PlainTime/Instant have
            // no calendar, so they (and any plain object) are a TypeError.
            if let HeapObj::Temporal { kind, .. } = self.heap.get(cv.heap_index()) {
                return if matches!(kind, 1 | 3 | 5 | 6 | 7) {
                    Ok(())
                } else {
                    Err(Thrown("TypeError: value is not a valid calendar".into()))
                };
            }
            if self.heap.is_str_like(cv.heap_index()) {
                let s = self.heap.str_cow(cv.heap_index()).unwrap().into_owned();
                return match calendar_id_from_string(&s) {
                    Some(id) if id.eq_ignore_ascii_case("iso8601") => Ok(()),
                    Some(id) => Err(Thrown(format!("RangeError: unsupported calendar \"{id}\""))),
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
    /// IDENTIFIER ("iso8601", ASCII-case-insensitive) — NOT a full ISO date /
    /// annotated string. (Those are only accepted by `withCalendar` and the
    /// property-bag `calendar` field, which keep using `validate_calendar_value`.)
    /// Non-string cases are identical to the general validator.
    pub(crate) fn validate_calendar_identifier(&mut self, cv: Value) -> Result<(), Thrown> {
        if cv.is_heap() && self.heap.is_str_like(cv.heap_index()) {
            let s = self.heap.str_cow(cv.heap_index()).unwrap().into_owned();
            return if s.trim().eq_ignore_ascii_case("iso8601") {
                Ok(())
            } else {
                Err(Thrown(format!("RangeError: \"{s}\" is not a valid calendar identifier")))
            };
        }
        self.validate_calendar_value(cv)
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
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "toJSON" => Ok(Some(self.alloc_str(year_month_string(y, m)))),
            "toString" => {
                // calendarName "always"/"critical" includes the reference ISO day.
                let suf = self.calendar_name_suffix(a0)?;
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
                // ISO PlainYearMonth equality includes the reference ISO day.
                let o = self.to_plain_year_month(a0)?;
                Ok(Some(Value::bool((y, m, rd) == (o.0, o.1, o.2))))
            }
            "with" => {
                self.reject_temporal_like(a0)?;
                // Field reads (observable getters) happen in alphabetical key order
                // (month, monthCode, year), all BEFORE reading the options bag.
                let mf = self.read_month_field_raw(a0)?;
                let yf = self.opt_int_field(a0, "year")?;
                if yf.is_none() && mf.is_none() {
                    return Err(Thrown(
                        "TypeError: with() requires at least one recognized property".into(),
                    ));
                }
                let ny = yf.unwrap_or(y);
                let month_valid = mf.map(|(_, v)| v).unwrap_or(true);
                let mut nm = mf.map(|(mm, _)| mm).unwrap_or(m);
                // month uses ToPositiveIntegerWithTruncation: a value below 1 is rejected
                // during field preparation, BEFORE the options bag is read.
                if nm < 1 {
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
                    // "constrain" clamps only the upper bound.
                    nm = nm.min(12);
                }
                Ok(Some(self.make_plain_year_month(ny, nm, 1)?))
            }
            "add" | "subtract" => {
                let dur = self.to_duration(a0)?;
                // The overflow option is still validated (constrain/reject/RangeError
                // on bad values) and read before the algorithmic range check below.
                let _ = self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
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
                let ref_day = if op_sign < 0 { days_in_month(y, m) } else { 1 };
                let (ny, nm, _nd) = self.date_add(y, m, ref_day, &dur, sign);
                Ok(Some(self.make_plain_year_month(ny, nm, 1)?))
            }
            "until" | "since" => {
                let o = self.to_plain_year_month(a0)?;
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
                let from = y * 12 + (m - 1);
                let to = o.0 * 12 + (o.1 - 1);
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
                        let r2y = (d_months / 12 / inc_i + s) * inc_i;
                        (y + r2y) * 12 + (m - 1)
                    } else if largest == "year" {
                        // Years split out first; only the months remainder is rounded.
                        let years = d_months / 12;
                        let r2m = (d_months % 12 / inc_i + s) * inc_i;
                        y * 12 + (m - 1) + years * 12 + r2m
                    } else {
                        let r2m = (d_months / inc_i + s) * inc_i;
                        y * 12 + (m - 1) + r2m
                    };
                    let (cy, cm) = (cand_idx.div_euclid(12), cand_idx.rem_euclid(12) + 1);
                    if !iso_date_in_range(cy, cm, 1) {
                        return Err(Thrown(
                            "RangeError: rounded PlainYearMonth difference is outside the representable range"
                                .into(),
                        ));
                    }
                }
                let mut f = [0i64; 10];
                if largest == "year" {
                    if smallest == "year" {
                        // Round the whole-year count to the increment (a year = 12 mo).
                        f[0] = round_increment(total_months as i128, 12 * inc, &mode) as i64 / 12;
                    } else {
                        // Split years out FIRST, then round ONLY the months remainder
                        // to the increment (years aren't rounded); carry across 12 if
                        // the rounded remainder overflows. (Trunc division keeps the
                        // sign for `since`'s negative difference.)
                        let years = total_months / 12;
                        let rem = total_months % 12;
                        let rm = round_increment(rem as i128, inc, &mode) as i64;
                        f[0] = years + rm / 12;
                        f[1] = rm % 12;
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
                let cd = day.min(days_in_month(y, m));
                Ok(Some(self.make_plain_date(y, m, cd)?))
            }
            "getISOFields" => {
                let cal = self.alloc_str("iso8601".to_string());
                let mut o = ObjMap::new();
                o.set("isoYear", Value::num(y as f64));
                o.set("isoMonth", Value::num(m as f64));
                o.set("isoDay", Value::num(rd as f64));
                o.set("calendar", cal);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Object(o)))))
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

    pub(crate) fn pmd_fields(&self, idx: u32) -> Option<(i64, i64, i64)> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 6, fields } => {
                Some((fields[0], fields[1], fields[2]))
            }
            _ => None,
        }
    }

    pub(crate) fn to_plain_month_day(&mut self, v: Value) -> Result<(i64, i64, i64), Thrown> {
        self.to_plain_month_day_overflow(v, None)
    }

    pub(crate) fn to_plain_month_day_overflow(
        &mut self,
        v: Value,
        options: Option<Value>,
    ) -> Result<(i64, i64, i64), Thrown> {
        if v.is_heap() {
            if let Some(t) = self.pmd_fields(v.heap_index()) {
                if let Some(o) = options {
                    self.read_overflow(o)?;
                }
                return Ok(t);
            }
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                if !temporal_string_ok(&s, true, true) {
                    return Err(Thrown(format!("RangeError: invalid month-day string '{s}'")));
                }
                return parse_iso_month_day(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid month-day string '{s}'")));
            }
            if self.is_object_value(v) {
                self.validate_iso_calendar_field(v)?;
                // Alphabetical field order: day, month, monthCode (raw), year.
                let d_opt = self.opt_int_field(v, "day")?;
                let m_raw = self.read_month_field_raw(v)?;
                // The reference `year` field is read (and finite-checked, rejecting
                // ±Infinity/NaN) even though this ISO engine always stores 1972 as
                // the reference ISO year. Required-field presence + this year coercion
                // precede a calendar-invalid monthCode's RangeError.
                let year_field = self.opt_int_field(v, "year")?;
                if m_raw.is_none() || d_opt.is_none() {
                    return Err(Thrown(
                        "TypeError: PlainMonthDay-like requires month and day".into(),
                    ));
                }
                let (m_val, m_valid) = m_raw.unwrap();
                // GetTemporalOverflowOption: read + validate options.overflow AFTER the
                // field GETs + year coercion (order-of-operations) but BEFORE the
                // calendar-invalid monthCode RangeError; absent options → constrain.
                let reject = if let Some(o) = options {
                    self.read_overflow(o)?
                } else {
                    false
                };
                if !m_valid {
                    return Err(Thrown(
                        "RangeError: monthCode is not valid for the ISO 8601 calendar".into(),
                    ));
                }
                let mut m = m_val;
                let mut d = d_opt.unwrap();
                // The supplied `year` (if any) decides whether the day overflows
                // (e.g. Feb-29 is valid only in a leap year); absent → 1972 (leap),
                // so a bare {month:2,day:29} stays valid. The stored reference year
                // is always 1972 regardless.
                let eff_year = year_field.unwrap_or(1972);
                if reject {
                    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(eff_year, m) {
                        return Err(Thrown("RangeError: month-day out of range".into()));
                    }
                } else {
                    // "constrain" clamps only the upper bound; month/day below 1 rejects.
                    if m < 1 || d < 1 {
                        return Err(Thrown("RangeError: month-day out of range".into()));
                    }
                    m = m.min(12);
                    d = d.min(days_in_month(eff_year, m));
                }
                return Ok((1972, m, d));
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
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "toJSON" => Ok(Some(self.alloc_str(format!("{m:02}-{d:02}")))),
            "toString" => {
                // calendarName "always"/"critical" includes the reference ISO year.
                let suf = self.calendar_name_suffix(a0)?;
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
                let o = self.to_plain_month_day(a0)?;
                Ok(Some(Value::bool((ry, m, d) == o)))
            }
            "with" => {
                self.reject_temporal_like(a0)?;
                // Field reads (observable getters) happen in alphabetical key order
                // (day, month, monthCode, year), all BEFORE reading the options bag.
                let df = self.opt_int_field(a0, "day")?;
                let mf = self.read_month_field_raw(a0)?;
                let yf = self.opt_int_field(a0, "year")?;
                if yf.is_none() && mf.is_none() && df.is_none() {
                    return Err(Thrown(
                        "TypeError: with() requires at least one recognized property".into(),
                    ));
                }
                let month_valid = mf.map(|(_, v)| v).unwrap_or(true);
                let mut nm = mf.map(|(mm, _)| mm).unwrap_or(m);
                let mut nd = df.unwrap_or(d);
                // month/day use ToPositiveIntegerWithTruncation: a value below 1 is
                // rejected during field preparation, BEFORE the options bag is read.
                if nm < 1 || nd < 1 {
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
                // PlainMonthDay(2,29).toPlainDate({year:2023}) → 2023-02-28).
                let cd = d.min(days_in_month(year, m));
                Ok(Some(self.make_plain_date(year, m, cd)?))
            }
            "getISOFields" => {
                let cal = self.alloc_str("iso8601".to_string());
                let mut o = ObjMap::new();
                o.set("isoYear", Value::num(ry as f64));
                o.set("isoMonth", Value::num(m as f64));
                o.set("isoDay", Value::num(d as f64));
                o.set("calendar", cal);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Object(o)))))
            }
            _ => Ok(None),
        }
    }

    // ── Intl ──

}
