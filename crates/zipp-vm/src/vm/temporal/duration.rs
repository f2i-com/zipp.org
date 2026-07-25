// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    /// Duration fields are float64-representable integers per the spec; the
    /// kind-0 heap record stores each field's f64 BITS in the i64 slots (the
    /// Vec<i64> shape — and thus GC tracing — is unchanged). Every reader goes
    /// through duration_fields; a missed raw `fields[i]` read would see bit
    /// patterns, i.e. obvious garbage, not subtle drift.
    pub(crate) fn make_duration(&mut self, f: [f64; 10]) -> Value {
        // Canonicalize -0.0 so sign probes and toString never see it.
        let bits =
            f.iter().map(|&x| (if x == 0.0 { 0.0f64 } else { x }).to_bits() as i64).collect();
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 0, fields: bits });
        if self.duration_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.duration_proto));
        }
        Value::heap(idx)
    }

    pub(crate) fn duration_fields(&self, idx: u32) -> Option<[f64; 10]> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 0, fields } => {
                let mut f = [0f64; 10];
                for (i, s) in f.iter_mut().enumerate() {
                    *s = f64::from_bits(*fields.get(i).unwrap_or(&0) as u64);
                }
                Some(f)
            }
            _ => None,
        }
    }

    /// The interim i64 view of a stored Duration for consumers that haven't
    /// been widened (saturating symmetrically; see dur_to_i64).
    pub(crate) fn duration_fields_i64(&self, idx: u32) -> Option<[i64; 10]> {
        self.duration_fields(idx).map(|f| f.map(Self::dur_to_i64))
    }

    /// ToIntegerIfIntegral for a Duration field: a Symbol or BigInt is a TypeError
    /// (ToNumber semantics; our plain to_number is lenient on BigInt), a user
    /// valueOf/toString is honoured, and the result must be a finite integer.
    pub(crate) fn duration_field(&mut self, v: Value) -> Result<f64, Thrown> {
        if v.is_heap()
            && matches!(
                self.heap.get(v.heap_index()),
                HeapObj::BigInt(_) | HeapObj::BigIntBig(_)
            )
        {
            return Err(Thrown("TypeError: Cannot convert a BigInt value to a number".into()));
        }
        let n = self.to_number_coerce(v)?;
        if !n.is_finite() || n.fract() != 0.0 {
            return Err(Thrown("RangeError: Temporal.Duration fields must be integers".into()));
        }
        Ok(n)
    }

    /// All non-zero fields must share a sign (else RangeError).
    pub(crate) fn validate_duration(&self, f: &[i64; 10]) -> Result<(), Thrown> {
        let mut sign = 0i64;
        for &x in f {
            let s = x.signum();
            if s != 0 {
                if sign == 0 {
                    sign = s;
                } else if s != sign {
                    return Err(Thrown(
                        "RangeError: mixed-sign values not allowed as duration fields".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Interim f64→i64 storage conversion while Duration fields are i64: `as`
    /// saturates a large negative value to i64::MIN, whose negation WRAPS back
    /// to i64::MIN — negated()/abs() would silently corrupt the sign and make
    /// `d.subtract(d.negated())` cancel to zero instead of overflowing. Clamp
    /// to ±i64::MAX so saturated magnitudes stay symmetric.
    pub(crate) fn dur_to_i64(x: f64) -> i64 {
        let v = x as i64;
        if v == i64::MIN { i64::MIN + 1 } else { v }
    }

    /// `new Temporal.Duration(y, mo, w, d, h, mi, s, ms, us, ns)` — integer fields.
    pub(crate) fn build_duration(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let mut ff = [0f64; 10];
        for (i, slot) in ff.iter_mut().enumerate() {
            let v = args.get(i).copied().unwrap_or(Value::UNDEFINED);
            if v != Value::UNDEFINED {
                *slot = self.duration_field(v)?;
            }
        }
        if !is_valid_duration(&ff) {
            return Err(Thrown("RangeError: Temporal.Duration value out of range".into()));
        }
        Ok(self.make_duration(ff))
    }

    /// ToTemporalDuration with the spec's EXACT field values: every field is a
    /// float64-representable integer, untouched by i64 saturation (a bag's
    /// microseconds of 4.5e21 stays 4.5e21). Consumers doing exact arithmetic
    /// (Instant/Duration add+subtract) convert per-field via `as i128`, which
    /// is exact on integer-valued f64s within i128 range.
    pub(crate) fn to_duration_f64(&mut self, v: Value) -> Result<[f64; 10], Thrown> {
        if let Some(idx) = (v.is_heap()).then(|| v.heap_index()) {
            if let Some(f) = self.duration_fields(idx) {
                return Ok(f);
            }
            if self.heap.is_str_like(idx) {
                let s = self.heap.str_cow(idx).unwrap().into_owned();
                let f = parse_iso_duration(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid duration string '{s}'")))?;
                let ff = f.map(|x| x as f64);
                // A parsed duration must also be in range (e.g. a days/seconds value
                // whose total exceeds 2^53 seconds is a RangeError).
                if !is_valid_duration(&ff) {
                    return Err(Thrown("RangeError: Temporal.Duration value out of range".into()));
                }
                return Ok(ff);
            }
            if self.is_object_value(v) {
                let mut ff = [0f64; 10];
                let mut any = false;
                // Read fields alphabetically (observable order); a Proxy bag is
                // accepted too (is_object_value, not just a plain Object).
                for &(i, name) in native::DURATION_FIELDS_ALPHA.iter() {
                    let pv = self.get_prop(v, name)?;
                    if pv != Value::UNDEFINED {
                        any = true;
                        ff[i] = self.duration_field(pv)?;
                    }
                }
                if !any {
                    return Err(Thrown(
                        "TypeError: object is not a valid Temporal.Duration-like".into(),
                    ));
                }
                if !is_valid_duration(&ff) {
                    return Err(Thrown("RangeError: Temporal.Duration value out of range".into()));
                }
                return Ok(ff);
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.Duration".into()))
    }

    /// ToTemporalDuration narrowed to the interim i64 record (saturating
    /// symmetrically; see dur_to_i64) for consumers that haven't been widened.
    pub(crate) fn to_duration(&mut self, v: Value) -> Result<[i64; 10], Thrown> {
        let ff = self.to_duration_f64(v)?;
        let f = ff.map(Self::dur_to_i64);
        self.validate_duration(&f)?;
        Ok(f)
    }

    pub(crate) fn duration_sign(f: &[i64; 10]) -> i64 {
        f.iter().map(|x| x.signum()).find(|&s| s != 0).unwrap_or(0)
    }

    /// Dispatch a Temporal instance method to the per-kind handler.
    pub(crate) fn temporal_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 0, .. } => self.duration_method(idx, name, args),
            HeapObj::Temporal { kind: 1, .. } => self.plain_date_method(idx, name, args),
            HeapObj::Temporal { kind: 2, .. } => self.plain_time_method(idx, name, args),
            HeapObj::Temporal { kind: 3, .. } => self.plain_date_time_method(idx, name, args),
            HeapObj::Temporal { kind: 4, .. } => self.instant_method(idx, name, args),
            HeapObj::Temporal { kind: 5, .. } => self.plain_year_month_method(idx, name, args),
            HeapObj::Temporal { kind: 6, .. } => self.plain_month_day_method(idx, name, args),
            HeapObj::Temporal { kind: 7, .. } => self.zoned_date_time_method(idx, name, args),
            _ => Ok(None),
        }
    }

    /// `Temporal.Duration.prototype` methods + getters not handled inline.
    pub(crate) fn duration_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let f = match self.duration_fields(idx) {
            Some(f) => f,
            None => return Ok(None),
        };
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "negated" => Ok(Some(self.make_duration(f.map(|x| -x)))),
            "abs" => Ok(Some(self.make_duration(f.map(|x| x.abs())))),
            "with" => {
                // Override the supplied fields (a partial-duration object), reading
                // them in the spec's alphabetical order.
                let mut nf = f;
                let mut any = false;
                for &(i, name) in native::DURATION_FIELDS_ALPHA.iter() {
                    let pv = self.get_prop(a0, name)?;
                    if pv != Value::UNDEFINED {
                        any = true;
                        nf[i] = self.duration_field(pv)?;
                    }
                }
                if !any {
                    return Err(Thrown("TypeError: with() requires a partial Duration object".into()));
                }
                if !is_valid_duration(&nf) {
                    return Err(Thrown("RangeError: Temporal.Duration value out of range".into()));
                }
                Ok(Some(self.make_duration(nf)))
            }
            "toJSON" => Ok(Some(self.alloc_str(duration_to_string(&f)))),
            "toString" => {
                let (_unit, digits, omit, mode) = self.time_precision(a0)?;
                if omit {
                    return Err(Thrown(
                        "RangeError: smallestUnit 'minute' is not valid for Duration.toString".into(),
                    ));
                }
                match duration_to_string_opts(&f, digits, &mode) {
                    Some(s) => Ok(Some(self.alloc_str(s))),
                    None => Err(Thrown(
                        "RangeError: rounded duration is outside the representable range".into(),
                    )),
                }
            }
            "valueOf" => {
                Err(Thrown("TypeError: Called Temporal.Duration.prototype.valueOf".into()))
            }
            "total" => {
                // arg: a unit string, or { unit, relativeTo }. GetTemporalRelativeToOption
                // is read BEFORE the unit (spec order).
                if a0 == Value::UNDEFINED {
                    return Err(Thrown("TypeError: total() requires an options argument".into()));
                }
                let is_string = a0.is_heap() && self.heap.is_str_like(a0.heap_index());
                if !is_string && !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: total() argument must be a string or object".into()));
                }
                let rel = if is_string { Value::UNDEFINED } else { self.get_prop(a0, "relativeTo")? };
                // GetTemporalRelativeToOption resolves the anchor (the full
                // ToRelativeTemporalObject read sequence) at the relativeTo slot,
                // BEFORE options.unit is read — even for a time unit, so
                // total({unit:"ns"}) against a ZonedDateTime at the limit still
                // throws on overflow.
                let anchor =
                    if rel != Value::UNDEFINED { Some(self.relative_to_dt(rel)?) } else { None };
                let unit_v = if is_string { a0 } else { self.get_prop(a0, "unit")? };
                if unit_v == Value::UNDEFINED {
                    return Err(Thrown("RangeError: unit is required".into()));
                }
                let unit = normalize_unit(&self.to_js_string(unit_v)?, "");
                if !DURATION_UNITS.contains(&unit.as_str()) {
                    return Err(Thrown(format!("RangeError: invalid unit: {unit}")));
                }
                // Years/months/weeks (in the value or as the requested unit) need a
                // calendar anchor; any other unit uses the time span directly.
                let needs_cal = f[0] != 0.0
                    || f[1] != 0.0
                    || f[2] != 0.0
                    || matches!(unit.as_str(), "year" | "month" | "week");
                if needs_cal && anchor.is_none() {
                    return Err(Thrown(
                        "RangeError: a relativeTo option is required for years, months, or weeks"
                            .into(),
                    ));
                }
                if let Some((start, zoned, off)) = anchor {
                    check_relative_target(start, &f, zoned, off, true)?;
                    if needs_cal {
                        // Calendar-relative totals stay on the i64 record (the
                        // calendar fields are small; exactness lives in the
                        // day+time paths).
                        return Ok(Some(Value::num(duration_total_relative(
                            f.map(Self::dur_to_i64),
                            start,
                            &unit,
                        )?)));
                    }
                    // TotalRelativeDuration with a zoned anchor and unit "day":
                    // NudgeToCalendarUnit materializes the NEXT day boundary
                    // (truncated days + sign) as an instant, which must be
                    // representable even when the result itself is exact.
                    if zoned && unit == "day" {
                        let diff_ns = dur_end_epoch_ns(start, &f) - dt_epoch_ns(start);
                        let s: i128 = if diff_ns < 0 { -1 } else { 1 };
                        let mut upper = [0i64; 10];
                        upper[3] = (diff_ns / DAY_NS + s) as i64;
                        let upper_wall = dt_add_dur(start, upper);
                        if (dt_epoch_ns(upper_wall) - off as i128).abs() > NS_MAX_INSTANT {
                            return Err(Thrown(
                                "RangeError: Temporal result is outside the representable range"
                                    .into(),
                            ));
                        }
                    }
                }
                let total_ns = dur_day_time_ns(&f);
                // Correctly-rounded single division of the exact rational (casting
                // total_ns to f64 first would double-round past 2^53).
                Ok(Some(Value::num(rational_to_f64(total_ns, unit_ns(&unit)))))
            }
            "round" => {
                // round(roundTo): a bare string is shorthand for { smallestUnit }.
                let (su_string, options) = if a0.is_heap() && self.heap.is_str_like(a0.heap_index()) {
                    (Some(normalize_unit(&self.to_js_string(a0)?, "")), Value::UNDEFINED)
                } else if a0 == Value::UNDEFINED {
                    return Err(Thrown("TypeError: round() requires an options argument".into()));
                } else if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: round() argument must be a string or object".into()));
                } else {
                    (None, a0)
                };
                // GetTemporalDurationRoundingSettings reads the options in this exact
                // order — largestUnit, relativeTo, roundingIncrement, roundingMode,
                // smallestUnit — and only then resolves defaults + runs validation.
                let lu_v = if options == Value::UNDEFINED {
                    Value::UNDEFINED
                } else {
                    self.get_prop(options, "largestUnit")?
                };
                let lu = if lu_v == Value::UNDEFINED {
                    None
                } else {
                    let s = normalize_unit(&self.to_js_string(lu_v)?, "auto");
                    if s == "auto" {
                        None
                    } else if !DURATION_UNITS.contains(&s.as_str()) {
                        return Err(Thrown(format!("RangeError: invalid largestUnit: {s}")));
                    } else {
                        Some(s)
                    }
                };
                // A relativeTo anchor enables calendar-unit rounding/balancing.
                // GetTemporalDurationRoundingSettings resolves the anchor (the full
                // ToRelativeTemporalObject read sequence) at the relativeTo slot,
                // BEFORE roundingIncrement/roundingMode/smallestUnit are read.
                let rel = if options == Value::UNDEFINED {
                    Value::UNDEFINED
                } else {
                    self.get_prop(options, "relativeTo")?
                };
                let anchor =
                    if rel != Value::UNDEFINED { Some(self.relative_to_dt(rel)?) } else { None };
                let inc = self.read_rounding_increment(options)?;
                let mode = if options == Value::UNDEFINED {
                    "halfExpand".to_string()
                } else {
                    self.read_rounding_mode(options, "halfExpand")?
                };
                let su = match su_string {
                    Some(s) => {
                        if !DURATION_UNITS.contains(&s.as_str()) {
                            return Err(Thrown(format!("RangeError: invalid smallestUnit: {s}")));
                        }
                        Some(s)
                    }
                    None => {
                        let su_v = if options == Value::UNDEFINED {
                            Value::UNDEFINED
                        } else {
                            self.get_prop(options, "smallestUnit")?
                        };
                        if su_v == Value::UNDEFINED {
                            None
                        } else {
                            let s = normalize_unit(&self.to_js_string(su_v)?, "");
                            if !DURATION_UNITS.contains(&s.as_str()) {
                                return Err(Thrown(format!("RangeError: invalid smallestUnit: {s}")));
                            }
                            Some(s)
                        }
                    }
                };
                // At least one of smallestUnit/largestUnit must be PROVIDED — an
                // explicit largestUnit "auto" counts (it resolves `lu` to None but is
                // not absent), so test the resolved smallestUnit / raw largestUnit.
                if su.is_none() && lu_v == Value::UNDEFINED {
                    return Err(Thrown(
                        "RangeError: at least one of smallestUnit or largestUnit is required".into(),
                    ));
                }
                let smallest = su.unwrap_or_else(|| "nanosecond".to_string());
                // largestUnit "auto" → the larger of smallestUnit and the duration's
                // own largest non-zero unit.
                let all = [
                    "year", "month", "week", "day", "hour", "minute", "second", "millisecond",
                    "microsecond", "nanosecond",
                ];
                let urank = |u: &str| all.iter().position(|&x| x == u).unwrap_or(9);
                let dur_largest =
                    all.iter().find(|&&u| f[urank(u)] != 0.0).copied().unwrap_or("nanosecond");
                let largest = lu.unwrap_or_else(|| {
                    if urank(dur_largest) < urank(&smallest) {
                        dur_largest.to_string()
                    } else {
                        smallest.clone()
                    }
                });
                if urank(&smallest) < urank(&largest) {
                    return Err(Thrown(
                        "RangeError: smallestUnit must not be larger than largestUnit".into(),
                    ));
                }
                // ValidateTemporalRoundingIncrement applies on BOTH paths (the
                // relativeTo branch used to skip it): a time-unit increment must divide
                // its next unit, and a calendar/day smallestUnit being balanced to a
                // coarser largestUnit forbids an increment greater than 1.
                if let Some(max) = max_increment(&smallest) {
                    if inc >= max || max % inc != 0 {
                        return Err(Thrown(
                            "RangeError: roundingIncrement must evenly divide the next unit".into(),
                        ));
                    }
                }
                if matches!(smallest.as_str(), "year" | "month" | "week" | "day")
                    && urank(&smallest) > urank(&largest)
                    && inc != 1
                {
                    return Err(Thrown(
                        "RangeError: roundingIncrement must be 1 when balancing a calendar unit"
                            .into(),
                    ));
                }
                if let Some((start, zoned, off)) = anchor {
                    check_relative_target(start, &f, zoned, off, true)?;
                    // Calendar-relative rounding stays on the i64 record; the
                    // exact range check above already ran on the f64 record.
                    let r = self.round_duration_relative(
                        f.map(Self::dur_to_i64),
                        start,
                        &smallest,
                        &largest,
                        inc,
                        &mode,
                        zoned,
                        off,
                    )?;
                    return Ok(Some(self.make_duration(r.map(|x| x as f64))));
                }
                // No relativeTo: calendar units require one.
                let cal = |u: &str| matches!(u, "year" | "month" | "week");
                if f[0] != 0.0 || f[1] != 0.0 || f[2] != 0.0 || cal(&smallest) || cal(&largest) {
                    return Err(Thrown(
                        "RangeError: a relativeTo option is required for years, months, or weeks".into(),
                    ));
                }
                let total_ns = dur_day_time_ns(&f);
                let inc_ns = unit_ns(&smallest) * inc;
                let rounded = round_increment(total_ns, inc_ns, &mode);
                let balanced = balance_duration_ns(rounded, &largest)?;
                Ok(Some(self.make_duration(balanced)))
            }
            "add" | "subtract" => {
                // The other operand keeps the spec's EXACT f64 record (a bag's
                // 4.5e21 microseconds must not saturate through i64); integer-
                // valued f64 → i128 conversion is exact, so the i128 total is
                // the true mathematical sum.
                let other = self.to_duration_f64(a0)?;
                let sign = if name == "add" { 1i64 } else { -1 };
                if f[0] != 0.0
                    || f[1] != 0.0
                    || f[2] != 0.0
                    || other[0] != 0.0
                    || other[1] != 0.0
                    || other[2] != 0.0
                {
                    return Err(Thrown(
                        "RangeError: a relativeTo option is required for years, months, or weeks".into(),
                    ));
                }
                let o_ns = (other[3] as i128) * DAY_NS
                    + (other[4] as i128) * 3_600_000_000_000
                    + (other[5] as i128) * 60_000_000_000
                    + (other[6] as i128) * 1_000_000_000
                    + (other[7] as i128) * 1_000_000
                    + (other[8] as i128) * 1_000
                    + (other[9] as i128);
                let total_ns = dur_day_time_ns(&f) + sign as i128 * o_ns;
                let existing_f =
                    |g: &[f64; 10]| (3..10).filter(|&i| g[i] != 0.0).map(|i| (i - 3) as i32).min().unwrap_or(6);
                let day_units =
                    ["day", "hour", "minute", "second", "millisecond", "microsecond", "nanosecond"];
                let largest = day_units[existing_f(&f).min(existing_f(&other)) as usize];
                // BalanceDuration → the result must be a valid Duration (its total
                // time, in seconds, below 2^53); balance_duration_ns enforces it.
                let balanced = balance_duration_ns(total_ns, largest)?;
                Ok(Some(self.make_duration(balanced)))
            }
            _ => Ok(None),
        }
    }

    // ── Temporal.PlainDate ──

}
