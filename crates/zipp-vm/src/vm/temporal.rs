#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    pub(crate) fn make_duration(&mut self, f: [i64; 10]) -> Value {
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 0, fields: f.to_vec() });
        if self.duration_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.duration_proto));
        }
        Value::heap(idx)
    }

    pub(crate) fn duration_fields(&self, idx: u32) -> Option<[i64; 10]> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 0, fields } => {
                let mut f = [0i64; 10];
                for (i, s) in f.iter_mut().enumerate() {
                    *s = *fields.get(i).unwrap_or(&0);
                }
                Some(f)
            }
            _ => None,
        }
    }

    /// ToIntegerIfIntegral for a Duration field: a Symbol or BigInt is a TypeError
    /// (ToNumber semantics; our plain to_number is lenient on BigInt), a user
    /// valueOf/toString is honoured, and the result must be a finite integer.
    pub(crate) fn duration_field(&mut self, v: Value) -> Result<f64, Thrown> {
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::BigInt(_)) {
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
        let f = ff.map(|x| x as i64);
        self.validate_duration(&f)?;
        Ok(self.make_duration(f))
    }

    /// ToTemporalDuration: a Duration clones; an object reads its duration fields;
    /// a string parses an ISO-8601 duration.
    pub(crate) fn to_duration(&mut self, v: Value) -> Result<[i64; 10], Thrown> {
        if let Some(idx) = (v.is_heap()).then(|| v.heap_index()) {
            if let Some(f) = self.duration_fields(idx) {
                return Ok(f);
            }
            if self.heap.is_str_like(idx) {
                let s = self.heap.str_cow(idx).unwrap().into_owned();
                let f = parse_iso_duration(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid duration string '{s}'")))?;
                // A parsed duration must also be in range (e.g. a days/seconds value
                // whose total exceeds 2^53 seconds is a RangeError).
                if !is_valid_duration(&f.map(|x| x as f64)) {
                    return Err(Thrown("RangeError: Temporal.Duration value out of range".into()));
                }
                return Ok(f);
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
                let f = ff.map(|x| x as i64);
                self.validate_duration(&f)?;
                return Ok(f);
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.Duration".into()))
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
                let mut nf = f.map(|x| x as f64);
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
                let nf = nf.map(|x| x as i64);
                self.validate_duration(&nf)?;
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
                Ok(Some(self.alloc_str(duration_to_string_opts(&f, digits, &mode))))
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
                let needs_cal = f[0] != 0
                    || f[1] != 0
                    || f[2] != 0
                    || matches!(unit.as_str(), "year" | "month" | "week");
                // ToRelativeTemporalObject: a provided relativeTo is resolved (and its
                // target instant validated) even for a time unit — total({unit:"ns"})
                // against a ZonedDateTime at the limit still throws on overflow.
                let anchor =
                    if rel != Value::UNDEFINED { Some(self.relative_to_dt(rel)?) } else { None };
                if needs_cal && anchor.is_none() {
                    return Err(Thrown(
                        "RangeError: a relativeTo option is required for years, months, or weeks"
                            .into(),
                    ));
                }
                if let Some((start, zoned)) = anchor {
                    check_relative_target(start, f, zoned)?;
                    if needs_cal {
                        return Ok(Some(Value::num(duration_total_relative(f, start, &unit)?)));
                    }
                }
                let total_ns = (f[3] as i128) * DAY_NS
                    + time_to_ns(&[f[4], f[5], f[6], f[7], f[8], f[9]]);
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
                let rel = if options == Value::UNDEFINED {
                    Value::UNDEFINED
                } else {
                    self.get_prop(options, "relativeTo")?
                };
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
                    all.iter().find(|&&u| f[urank(u)] != 0).copied().unwrap_or("nanosecond");
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
                if rel != Value::UNDEFINED {
                    let (start, zoned) = self.relative_to_dt(rel)?;
                    check_relative_target(start, f, zoned)?;
                    let r = self.round_duration_relative(f, start, &smallest, &largest, inc, &mode)?;
                    return Ok(Some(self.make_duration(r)));
                }
                // No relativeTo: calendar units require one.
                let cal = |u: &str| matches!(u, "year" | "month" | "week");
                if f[0] != 0 || f[1] != 0 || f[2] != 0 || cal(&smallest) || cal(&largest) {
                    return Err(Thrown(
                        "RangeError: a relativeTo option is required for years, months, or weeks".into(),
                    ));
                }
                let total_ns = (f[3] as i128) * DAY_NS
                    + time_to_ns(&[f[4], f[5], f[6], f[7], f[8], f[9]]);
                let inc_ns = unit_ns(&smallest) * inc;
                let rounded = round_increment(total_ns, inc_ns, &mode);
                let balanced = balance_duration_ns(rounded, &largest);
                if !is_valid_duration(&std::array::from_fn(|i| balanced[i] as f64)) {
                    return Err(Thrown("RangeError: Temporal.Duration value out of range".into()));
                }
                Ok(Some(self.make_duration(balanced)))
            }
            "add" | "subtract" => {
                let other = self.to_duration(a0)?;
                let sign = if name == "add" { 1i64 } else { -1 };
                if f[0] != 0 || f[1] != 0 || f[2] != 0 || other[0] != 0 || other[1] != 0 || other[2] != 0 {
                    return Err(Thrown(
                        "RangeError: a relativeTo option is required for years, months, or weeks".into(),
                    ));
                }
                let total_ns = (f[3] as i128) * DAY_NS
                    + time_to_ns(&[f[4], f[5], f[6], f[7], f[8], f[9]])
                    + sign as i128
                        * ((other[3] as i128) * DAY_NS
                            + time_to_ns(&[other[4], other[5], other[6], other[7], other[8], other[9]]));
                let existing =
                    |g: &[i64; 10]| (3..10).filter(|&i| g[i] != 0).map(|i| (i - 3) as i32).min().unwrap_or(6);
                let day_units =
                    ["day", "hour", "minute", "second", "millisecond", "microsecond", "nanosecond"];
                let largest = day_units[existing(&f).min(existing(&other)) as usize];
                let balanced = balance_duration_ns(total_ns, largest);
                // BalanceDuration → the result must be a valid Duration (its total
                // time, in seconds, below 2^53); else RangeError.
                if !is_valid_duration(&std::array::from_fn(|i| balanced[i] as f64)) {
                    return Err(Thrown("RangeError: Temporal.Duration value out of range".into()));
                }
                Ok(Some(self.make_duration(balanced)))
            }
            _ => Ok(None),
        }
    }

    // ── Temporal.PlainDate ──

    pub(crate) fn make_plain_date(&mut self, y: i64, m: i64, d: i64) -> Result<Value, Thrown> {
        if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) || !iso_date_in_range(y, m, d) {
            return Err(Thrown("RangeError: invalid ISO date".into()));
        }
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 1, fields: vec![y, m, d] });
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
            return Err(Thrown("TypeError: options must be an object or undefined".into()));
        }
        let v = self.get_prop(options, "overflow")?;
        if v == Value::UNDEFINED {
            return Ok(false);
        }
        match self.to_js_string(v)?.as_str() {
            "constrain" => Ok(false),
            "reject" => Ok(true),
            other => Err(Thrown(format!("RangeError: invalid overflow value: {other}"))),
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
    ) -> Result<(String, bool), Thrown> {
        if options != Value::UNDEFINED && !self.is_object_value(options) {
            return Err(Thrown("TypeError: options must be an object or undefined".into()));
        }
        self.opt_string(
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
        Ok((off, reject))
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
            return Err(Thrown("TypeError: options must be an object or undefined".into()));
        }
        // ToSecondsStringPrecision reads options in spec order — fractionalSecondDigits,
        // then roundingMode, then smallestUnit — casting each (the observable
        // get/.toString sequence the order-of-operations tests assert) before applying
        // precedence. smallestUnit, when present, wins over fractionalSecondDigits, but
        // fsd's read+cast+validation still occurs first.
        let fsd_v = self.get_prop(options, "fractionalSecondDigits")?;
        let fsd_result: (i128, i32, bool) = if fsd_v == Value::UNDEFINED {
            (1, -1, false)
        } else if !fsd_v.is_number() {
            // A string/null/boolean/bigint/object is ToString'd and must be "auto"
            // (a Symbol throws TypeError inside to_js_string).
            if self.to_js_string(fsd_v)? == "auto" {
                (1, -1, false)
            } else {
                return Err(Thrown(
                    "RangeError: fractionalSecondDigits must be 'auto' or 0..9".into(),
                ));
            }
        } else {
            // A genuine Number is floored into 0..9 (GetStringOrNumberOption).
            let n = self.to_number(fsd_v)?;
            if n.is_nan() {
                return Err(Thrown("RangeError: fractionalSecondDigits is NaN".into()));
            }
            let n = n.floor() as i64;
            if !(0..=9).contains(&n) {
                return Err(Thrown("RangeError: fractionalSecondDigits out of range".into()));
            }
            (10i128.pow(9 - n as u32), n as i32, false)
        };
        let mode = self.opt_string(
            options,
            "roundingMode",
            "trunc",
            &[
                "ceil", "floor", "trunc", "expand", "halfCeil", "halfFloor", "halfTrunc",
                "halfEven", "halfExpand",
            ],
        )?;
        let su_v = self.get_prop(options, "smallestUnit")?;
        if su_v != Value::UNDEFINED {
            let su = normalize_unit(&self.to_js_string(su_v)?, "");
            let (unit, digits, omit) = match su.as_str() {
                "minute" => (60_000_000_000i128, 0, true),
                "second" => (1_000_000_000, 0, false),
                "millisecond" => (1_000_000, 3, false),
                "microsecond" => (1_000, 6, false),
                "nanosecond" => (1, 9, false),
                _ => {
                    return Err(Thrown(format!(
                        "RangeError: invalid smallestUnit for toString: {su}"
                    )))
                }
            };
            return Ok((unit, digits, omit, mode));
        }
        Ok((fsd_result.0, fsd_result.1, fsd_result.2, mode))
    }

    /// The calendar annotation suffix for a toString() per the `calendarName`
    /// option: "always" → "[u-ca=iso8601]", "critical" → "[!u-ca=iso8601]",
    /// "auto" (iso8601 is the default calendar, so omitted) / "never" → "".
    pub(crate) fn calendar_name_suffix(&mut self, options: Value) -> Result<String, Thrown> {
        if options == Value::UNDEFINED {
            return Ok(String::new());
        }
        if !self.is_object_value(options) {
            return Err(Thrown("TypeError: options must be an object or undefined".into()));
        }
        let cn = self.opt_string(
            options,
            "calendarName",
            "auto",
            &["auto", "always", "never", "critical"],
        )?;
        Ok(match cn.as_str() {
            "always" => "[u-ca=iso8601]".to_string(),
            "critical" => "[!u-ca=iso8601]".to_string(),
            _ => String::new(),
        })
    }

    pub(crate) fn to_plain_date(&mut self, v: Value) -> Result<(i64, i64, i64), Thrown> {
        self.to_plain_date_overflow(v, false)
    }

    /// ToTemporalDate with an overflow mode (constrain clamps; reject throws on
    /// out-of-range fields). A real PlainDate clones; a string parses.
    pub(crate) fn to_plain_date_overflow(
        &mut self,
        v: Value,
        reject: bool,
    ) -> Result<(i64, i64, i64), Thrown> {
        if v.is_heap() {
            if let Some(t) = self.plain_date_fields(v.heap_index()) {
                return Ok(t);
            }
            // A ZonedDateTime or PlainDateTime yields its calendar date.
            if let HeapObj::Temporal { kind, .. } = self.heap.get(v.heap_index()) {
                let date = match kind {
                    7 => Some(self.zdt_local(v.heap_index())),
                    3 => self.pdt_fields(v.heap_index()),
                    _ => None,
                };
                if let Some(f) = date {
                    return Ok((f[0], f[1], f[2]));
                }
            }
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                if !temporal_string_ok(&s, true, true) {
                    return Err(Thrown(format!("RangeError: invalid date string '{s}'")));
                }
                let (y, m, d) = parse_iso_date(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid date string '{s}'")))?;
                if !iso_date_in_range(y, m, d) {
                    return Err(Thrown(format!(
                        "RangeError: date '{s}' is outside the representable range"
                    )));
                }
                return Ok((y, m, d));
            }
            if self.is_object_value(v) {
                self.validate_iso_calendar_field(v)?;
                let y_opt = self.opt_int_field(v, "year")?;
                let m_opt = self.read_month_field(v)?; // monthCode or month
                let d_opt = self.opt_int_field(v, "day")?;
                if y_opt.is_none() || m_opt.is_none() || d_opt.is_none() {
                    return Err(Thrown("TypeError: PlainDate-like requires year, month, day".into()));
                }
                let (y, mut m, mut d) = (y_opt.unwrap(), m_opt.unwrap(), d_opt.unwrap());
                if reject {
                    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
                        return Err(Thrown("RangeError: invalid date fields".into()));
                    }
                } else {
                    // "constrain" clamps only the UPPER bound; a month/day below 1 is
                    // a hard floor that always rejects (RegulateISODate is reached only
                    // after the fields are validated >= 1).
                    if m < 1 || d < 1 {
                        return Err(Thrown("RangeError: invalid date fields".into()));
                    }
                    m = m.min(12);
                    d = d.min(days_in_month(y, m));
                }
                if !iso_date_in_range(y, m, d) {
                    return Err(Thrown("RangeError: date is outside the representable range".into()));
                }
                return Ok((y, m, d));
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.PlainDate".into()))
    }

    /// Round the date difference from `d1` to `d2` to `smallest` (a calendar unit:
    /// year/month/week), then balance up to `largest`. NudgeToCalendarUnit: the
    /// whole count of the smallest unit, plus the fraction of the way to the next
    /// (measured in days against the anchor calendar), rounded per `mode`. Assumes
    /// roundingIncrement 1 (the spec disallows >1 for calendar units).
    pub(crate) fn round_relative_date_diff(
        &self,
        d1: (i64, i64, i64),
        d2: (i64, i64, i64),
        smallest: &str,
        largest: &str,
        inc: i128,
        mode: &str,
    ) -> Result<[i64; 4], Thrown> {
        let rank =
            |u: &str| ["year", "month", "week", "day"].iter().position(|&x| x == u).unwrap_or(3);
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
        let base = difference_iso_date(d1, d2, largest);
        // smallestUnit = week: difference_iso_date dumps the sub-month remainder into
        // DAYS (weeks = 0 when largestUnit > week), so derive the whole-week count from
        // the full sub-week day span instead of the (zeroed) week field.
        let sval = if si == 2 { (base[2] * 7 + base[3]) / 7 } else { base[si] };
        let mk = |k: i64| -> [i64; 10] {
            let mut dur = [0i64; 10];
            dur[..si].copy_from_slice(&base[..si]);
            dur[si] = k;
            dur
        };
        let r1 = round_increment(sval as i128, inc, "trunc") as i64;
        let r2 = r1 + inc as i64 * sign;
        let lower = self.date_add(d1.0, d1.1, d1.2, &mk(r1), 1);
        let ld = iso_to_epoch_days(lower.0, lower.1, lower.2);
        // The r2 endpoint is a CalendarDateAdd(constrain) that must lie within the
        // ISO date limits — a huge increment can push it past the range (RangeError).
        let upper = self.date_add(d1.0, d1.1, d1.2, &mk(r2), 1);
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
            let progress = if denom != 0.0 { (e2 - ld) as f64 / denom } else { 0.0 };
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
        let end = self.date_add(d1.0, d1.1, d1.2, &mk(picked), 1);
        Ok(difference_iso_date(d1, end, largest))
    }

    /// `date ± duration` (date units constrain day; time units fold to whole days).
    pub(crate) fn date_add(&self, y: i64, m: i64, d: i64, dur: &[i64; 10], sign: i64) -> (i64, i64, i64) {
        self.date_add_overflow(y, m, d, dur, sign, false).unwrap()
    }

    /// `date ± duration` with an overflow mode. The year+month step can land the
    /// day past the new month's length: "constrain" clamps it, "reject" throws.
    /// Weeks/days/time then add via exact epoch-day math.
    pub(crate) fn date_add_overflow(
        &self,
        y: i64,
        m: i64,
        d: i64,
        dur: &[i64; 10],
        sign: i64,
        reject: bool,
    ) -> Result<(i64, i64, i64), Thrown> {
        let total_months = (y + dur[0] * sign) * 12 + (m - 1) + dur[1] * sign;
        let ny = total_months.div_euclid(12);
        let nm = total_months.rem_euclid(12) + 1;
        if reject && d > days_in_month(ny, nm) {
            return Err(Thrown("RangeError: date arithmetic overflows the month".into()));
        }
        let nd = d.min(days_in_month(ny, nm));
        let time_ns = (dur[4] as i128) * 3_600_000_000_000
            + (dur[5] as i128) * 60_000_000_000
            + (dur[6] as i128) * 1_000_000_000
            + (dur[7] as i128) * 1_000_000
            + (dur[8] as i128) * 1_000
            + (dur[9] as i128);
        let extra_days = (time_ns / 86_400_000_000_000) as i64;
        let ed = iso_to_epoch_days(ny, nm, nd) + (dur[2] * 7 + dur[3] + extra_days) * sign;
        Ok(epoch_days_to_iso(ed))
    }

    pub(crate) fn plain_date_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let (y, m, d) = match self.plain_date_fields(idx) {
            Some(t) => t,
            None => return Ok(None),
        };
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "toJSON" => Ok(Some(self.alloc_str(iso_date_string(y, m, d)))),
            "toString" => {
                let suf = self.calendar_name_suffix(a0)?;
                Ok(Some(self.alloc_str(format!("{}{}", iso_date_string(y, m, d), suf))))
            }
            "valueOf" => Err(Thrown("TypeError: Called Temporal.PlainDate.prototype.valueOf".into())),
            "equals" => {
                let other = self.to_plain_date(a0)?;
                Ok(Some(Value::bool((y, m, d) == other)))
            }
            // The ISO reference is canonical: day 1 for a year-month, year 1972 for a
            // month-day — NOT the source date's day/year.
            "toPlainYearMonth" => Ok(Some(self.make_plain_year_month(y, m, 1)?)),
            "toPlainMonthDay" => Ok(Some(self.make_plain_month_day(m, d, 1972)?)),
            "toPlainDateTime" => {
                // Combine this date with a time (ToTemporalTime; default midnight).
                let t = if a0 == Value::UNDEFINED { [0i64; 6] } else { self.to_plain_time(a0)? };
                Ok(Some(self.make_plain_date_time([y, m, d, t[0], t[1], t[2], t[3], t[4], t[5]])?))
            }
            "withCalendar" => {
                self.validate_calendar_value(a0)?;
                Ok(Some(self.make_plain_date(y, m, d)?))
            }
            "toZonedDateTime" => {
                let (id, offset) = self.parse_tz_arg(a0)?;
                let time = if a0.is_heap()
                    && matches!(self.heap.get(a0.heap_index()), HeapObj::Object(_))
                {
                    let pt = self.get_prop(a0, "plainTime")?;
                    if pt == Value::UNDEFINED {
                        [0i64; 6]
                    } else {
                        self.to_plain_time(pt)?
                    }
                } else {
                    [0i64; 6]
                };
                let local = (iso_to_epoch_days(y, m, d) as i128) * DAY_NS + time_to_ns(&time);
                Ok(Some(self.alloc_zdt(local - offset as i128, offset, id)))
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
                let ny = yf.unwrap_or(y);
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
                if !reject {
                    // "constrain" clamps only the UPPER bound.
                    nm = nm.min(12);
                    nd = nd.min(days_in_month(ny, nm));
                }
                Ok(Some(self.make_plain_date(ny, nm, nd)?))
            }
            "add" | "subtract" => {
                let dur = self.to_duration(a0)?;
                let reject = self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                let sign = if name == "add" { 1 } else { -1 };
                let (ny, nm, nd) = self.date_add_overflow(y, m, d, &dur, sign, reject)?;
                Ok(Some(self.make_plain_date(ny, nm, nd)?))
            }
            "until" | "since" => {
                let other = self.to_plain_date(a0)?;
                let opts = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if opts != Value::UNDEFINED && !self.is_object_value(opts) {
                    return Err(Thrown("TypeError: options must be an object or undefined".into()));
                }
                let date_units = &[
                    "auto", "year", "years", "month", "months", "week", "weeks", "day", "days",
                ];
                // GetDifferenceSettings: smallestUnit (default "day"), largestUnit
                // (default "auto" → the larger of smallestUnit and "day").
                let smallest =
                    normalize_unit(&self.opt_string(opts, "smallestUnit", "day", date_units)?, "day");
                let largest_raw =
                    normalize_unit(&self.opt_string(opts, "largestUnit", "auto", date_units)?, "auto");
                let order = ["year", "month", "week", "day"];
                let rank = |u: &str| order.iter().position(|&x| x == u).unwrap_or(3);
                let largest = if largest_raw == "auto" {
                    if rank(&smallest) < 3 { smallest.clone() } else { "day".to_string() }
                } else {
                    largest_raw
                };
                if rank(&smallest) < rank(&largest) {
                    return Err(Thrown(
                        "RangeError: smallestUnit is larger than largestUnit".into(),
                    ));
                }
                let inc = self.read_rounding_increment(opts)?;
                let mode = self.read_rounding_mode(opts, "trunc")?;
                // since = negate(until): always compute the forward (this → other)
                // difference with a sign-negated rounding mode, then negate the result.
                // (Swapping operands for `since` would anchor the day-of-month borrow on
                // the wrong date.)
                let (d1, d2) = ((y, m, d), other);
                let eff = if name == "since" { negate_mode(&mode) } else { mode.clone() };
                let mut f = [0i64; 10];
                // The day field rounds to the increment; a calendar smallestUnit
                // (year/month/week) rounds the fractional remainder against the
                // anchor calendar (NudgeToCalendarUnit) and balances to largestUnit.
                let si = rank(&smallest);
                if si == 3 {
                    let diff = difference_iso_date(d1, d2, &largest);
                    f[..4].copy_from_slice(&diff);
                    f[3] = round_increment(f[3] as i128, inc, &eff) as i64;
                } else {
                    let r = self.round_relative_date_diff(d1, d2, &smallest, &largest, inc, &eff)?;
                    f[..4].copy_from_slice(&r);
                }
                if name == "since" {
                    f.iter_mut().for_each(|x| *x = -*x);
                }
                Ok(Some(self.make_duration(f)))
            }
            "getISOFields" => {
                let cal = self.alloc_str("iso8601".to_string());
                let mut o = ObjMap::new();
                o.set("isoYear", Value::num(y as f64));
                o.set("isoMonth", Value::num(m as f64));
                o.set("isoDay", Value::num(d as f64));
                o.set("calendar", cal);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Object(o)))))
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
        if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::BigInt(_)) {
            return Err(Thrown("TypeError: Cannot convert a BigInt value to a number".into()));
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
    pub(crate) fn read_rounding_mode(&mut self, opts: Value, default: &str) -> Result<String, Thrown> {
        self.opt_string(
            opts,
            "roundingMode",
            default,
            &[
                "ceil", "floor", "trunc", "expand", "halfCeil", "halfFloor", "halfTrunc",
                "halfEven", "halfExpand",
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
            return Err(Thrown("TypeError: with() requires a property-bag object".into()));
        }
        if arg.is_heap() {
            if let HeapObj::Temporal { .. } = self.heap.get(arg.heap_index()) {
                return Err(Thrown("TypeError: with() does not accept a Temporal object".into()));
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
            return Err(Thrown(format!("RangeError: {key} property must be a finite number")));
        }
        Ok(Some(n.trunc() as i64))
    }

    // ── Temporal.PlainTime ──

    pub(crate) fn make_plain_time(&mut self, f: [i64; 6]) -> Result<Value, Thrown> {
        if !(0..24).contains(&f[0])
            || !(0..60).contains(&f[1])
            || !(0..60).contains(&f[2])
            || !(0..1000).contains(&f[3])
            || !(0..1000).contains(&f[4])
            || !(0..1000).contains(&f[5])
        {
            return Err(Thrown("RangeError: invalid time value".into()));
        }
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 2, fields: f.to_vec() });
        if self.plaintime_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.plaintime_proto));
        }
        Ok(Value::heap(idx))
    }

    pub(crate) fn plain_time_fields(&self, idx: u32) -> Option<[i64; 6]> {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 2, fields } => {
                let mut f = [0i64; 6];
                for (i, s) in f.iter_mut().enumerate() {
                    *s = *fields.get(i).unwrap_or(&0);
                }
                Some(f)
            }
            _ => None,
        }
    }

    pub(crate) fn to_plain_time(&mut self, v: Value) -> Result<[i64; 6], Thrown> {
        self.to_plain_time_overflow(v, false)
    }

    pub(crate) fn to_plain_time_overflow(
        &mut self,
        v: Value,
        reject: bool,
    ) -> Result<[i64; 6], Thrown> {
        if v.is_heap() {
            if let Some(f) = self.plain_time_fields(v.heap_index()) {
                return Ok(f);
            }
            // A ZonedDateTime or PlainDateTime yields its wall-clock time.
            if let HeapObj::Temporal { kind, .. } = self.heap.get(v.heap_index()) {
                let f = match kind {
                    7 => Some(self.zdt_local(v.heap_index())),
                    3 => self.pdt_fields(v.heap_index()),
                    _ => None,
                };
                if let Some(f) = f {
                    return Ok([f[3], f[4], f[5], f[6], f[7], f[8]]);
                }
            }
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                if !temporal_string_ok(&s, true, false) {
                    return Err(Thrown(format!("RangeError: invalid time string '{s}'")));
                }
                return parse_temporal_time(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid time string '{s}'")));
            }
            if self.is_object_value(v) {
                let names =
                    ["hour", "minute", "second", "millisecond", "microsecond", "nanosecond"];
                let maxes = [23, 59, 59, 999, 999, 999];
                let mut f = [0i64; 6];
                let mut any = false;
                for (i, nm) in names.iter().enumerate() {
                    if let Some(x) = self.opt_int_field(v, nm)? {
                        any = true;
                        if reject {
                            if x < 0 || x > maxes[i] {
                                return Err(Thrown(format!("RangeError: {nm} out of range")));
                            }
                            f[i] = x;
                        } else {
                            f[i] = x.clamp(0, maxes[i]);
                        }
                    }
                }
                // ToTemporalTimeRecord: a property bag with NO recognized time field
                // (hour/minute/second/ms/us/ns) is not a valid PlainTime-like — a
                // TypeError, not a silent default to 00:00:00.
                if !any {
                    return Err(Thrown(
                        "TypeError: object has no recognized Temporal.PlainTime fields".into(),
                    ));
                }
                return Ok(f);
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.PlainTime".into()))
    }

    pub(crate) fn plain_time_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let f = match self.plain_time_fields(idx) {
            Some(f) => f,
            None => return Ok(None),
        };
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        match name {
            "toJSON" => Ok(Some(self.alloc_str(time_string(&f)))),
            "toString" => {
                let (unit, digits, omit, mode) = self.time_precision(a0)?;
                let rounded = round_increment(time_to_ns(&f), unit, &mode).rem_euclid(DAY_NS);
                let t = ns_to_time(rounded);
                Ok(Some(self.alloc_str(format_time_part(&t, digits, omit))))
            }
            "valueOf" => Err(Thrown("TypeError: Called Temporal.PlainTime.prototype.valueOf".into())),
            "equals" => {
                let o = self.to_plain_time(a0)?;
                Ok(Some(Value::bool(f == o)))
            }
            "with" => {
                self.reject_temporal_like(a0)?;
                let names =
                    ["hour", "minute", "second", "millisecond", "microsecond", "nanosecond"];
                let maxes = [23, 59, 59, 999, 999, 999];
                // Read all the time fields (observable getters) BEFORE the options bag.
                let mut raw: [Option<i64>; 6] = [None; 6];
                let mut any = false;
                for (i, nm) in names.iter().enumerate() {
                    if let Some(x) = self.opt_int_field(a0, nm)? {
                        raw[i] = Some(x);
                        any = true;
                    }
                }
                if !any {
                    return Err(Thrown("TypeError: with() requires a partial time object".into()));
                }
                let reject = self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                let mut nf = f;
                for (i, slot) in raw.iter().enumerate() {
                    if let Some(x) = *slot {
                        nf[i] = if reject { x } else { x.clamp(0, maxes[i]) };
                    }
                }
                Ok(Some(self.make_plain_time(nf)?))
            }
            "add" | "subtract" => {
                let dur = self.to_duration(a0)?;
                let sign: i128 = if name == "add" { 1 } else { -1 };
                let dur_ns = ((dur[4] as i128) * 3_600_000_000_000
                    + (dur[5] as i128) * 60_000_000_000
                    + (dur[6] as i128) * 1_000_000_000
                    + (dur[7] as i128) * 1_000_000
                    + (dur[8] as i128) * 1_000
                    + (dur[9] as i128))
                    * sign;
                let total = (time_to_ns(&f) + dur_ns).rem_euclid(86_400_000_000_000);
                Ok(Some(self.make_plain_time(ns_to_time(total))?))
            }
            "until" | "since" => {
                let o = self.to_plain_time(a0)?;
                let a1 = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let (largest, smallest, inc, mode) = self.read_time_diff_options(a1, "hour")?;
                let diff = if name == "until" {
                    time_to_ns(&o) - time_to_ns(&f)
                } else {
                    time_to_ns(&f) - time_to_ns(&o)
                };
                let inc_ns = unit_ns(&smallest) * inc;
                let rounded = round_increment(diff, inc_ns, &mode);
                Ok(Some(self.make_duration(balance_duration_ns(rounded, &largest))))
            }
            "round" => {
                let (su, inc, mode) = self.read_round_options(
                    a0,
                    &["hour", "minute", "second", "millisecond", "microsecond", "nanosecond"],
                    true,
                )?;
                let ns = time_to_ns(&f);
                let inc_ns = unit_ns(&su) * inc;
                let rounded = round_increment(ns, inc_ns, &mode).rem_euclid(DAY_NS);
                Ok(Some(self.make_plain_time(ns_to_time(rounded))?))
            }
            "getISOFields" => {
                let cal = self.alloc_str("iso8601".to_string());
                let mut o = ObjMap::new();
                let names = [
                    "isoHour",
                    "isoMinute",
                    "isoSecond",
                    "isoMillisecond",
                    "isoMicrosecond",
                    "isoNanosecond",
                ];
                for (i, nm) in names.iter().enumerate() {
                    o.set(nm, Value::num(f[i] as f64));
                }
                o.set("calendar", cal);
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Object(o)))))
            }
            _ => Ok(None),
        }
    }

    // ── Temporal.PlainDateTime ──

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
                self.validate_iso_calendar_field(v)?;
                let mut f = [0i64; 9];
                let mut have_date = [false; 3];
                if let Some(x) = self.opt_int_field(v, "year")? {
                    f[0] = x;
                    have_date[0] = true;
                }
                if let Some(x) = self.read_month_field(v)? {
                    // monthCode ("M11") or month
                    f[1] = x;
                    have_date[1] = true;
                }
                if let Some(x) = self.opt_int_field(v, "day")? {
                    f[2] = x;
                    have_date[2] = true;
                }
                let time_names =
                    ["hour", "minute", "second", "millisecond", "microsecond", "nanosecond"];
                for (i, nm) in time_names.iter().enumerate() {
                    if let Some(x) = self.opt_int_field(v, nm)? {
                        f[3 + i] = x;
                    }
                }
                if !have_date.iter().all(|&b| b) {
                    return Err(Thrown("TypeError: PlainDateTime-like requires year, month, day".into()));
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
                return Ok(f);
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.PlainDateTime".into()))
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
                let (unit, digits, omit, mode) = self.time_precision(a0)?;
                let suf = self.calendar_name_suffix(a0)?;
                let rounded = round_increment(time_to_ns(&time), unit, &mode);
                let carry = rounded.div_euclid(DAY_NS) as i64;
                let t = ns_to_time(rounded.rem_euclid(DAY_NS));
                let (ny, nm, nd) =
                    epoch_days_to_iso(iso_to_epoch_days(date[0], date[1], date[2]) + carry);
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
                Ok(Some(self.alloc_zdt(local - offset as i128, offset, id)))
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
                // smallestUnit default "nanosecond"; largestUnit default "auto" →
                // the larger of smallestUnit and "day".
                let smallest = normalize_unit(
                    &self.opt_string(opts, "smallestUnit", "nanosecond", all_units)?,
                    "nanosecond",
                );
                let largest_raw =
                    normalize_unit(&self.opt_string(opts, "largestUnit", "auto", all_units)?, "auto");
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
                let inc = self.read_rounding_increment(opts)?;
                let mode = self.read_rounding_mode(opts, "trunc")?;
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
                    balance_duration_ns(rounded, &largest)
                } else if matches!(smallest.as_str(), "year" | "month" | "week") {
                    round_relative_datetime_diff(dt1, dt2, &smallest, &largest, inc, &eff)?
                } else {
                    round_datetime_diff_daytime(dt1, df, &smallest, &largest, inc, &eff)
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
                Ok(Some(Value::heap(self.heap.alloc(HeapObj::Object(o)))))
            }
            _ => Ok(None),
        }
    }

    // ── Temporal.Instant ──

    /// Current wall-clock time as nanoseconds since the Unix epoch.
    pub(crate) fn now_epoch_ns() -> i128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i128)
            .unwrap_or(0)
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
    pub(crate) fn make_zoned_date_time(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let _gc = self.gc_lock_guard();
        let ns = self.to_bigint(args.first().copied().unwrap_or(Value::UNDEFINED))?;
        if ns.abs() > 8_640_000_000_000_000_000_000 {
            return Err(Thrown("RangeError: ZonedDateTime outside the supported range".into()));
        }
        let tzarg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        if tzarg == Value::UNDEFINED {
            return Err(Thrown("TypeError: Temporal.ZonedDateTime requires a time zone".into()));
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

    /// The UTC offset (nanoseconds) of a ZonedDateTime instance.
    pub(crate) fn zdt_offset_ns(&self, idx: u32) -> i64 {
        match self.heap.get(idx) {
            HeapObj::Temporal { kind: 7, fields } => fields.get(2).copied().unwrap_or(0),
            _ => 0,
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
                Ok(Some(self.make_plain_date_time(f)?))
            }
            "toPlainDate" => {
                let f = self.zdt_local(idx);
                Ok(Some(self.make_plain_date(f[0], f[1], f[2])?))
            }
            "toPlainTime" => {
                let f = self.zdt_local(idx);
                Ok(Some(self.make_plain_time([f[3], f[4], f[5], f[6], f[7], f[8]])?))
            }
            "startOfDay" => {
                let off = self.zdt_offset_ns(idx) as i128;
                let local = self.zdt_epoch_ns(idx).unwrap_or(0) + off;
                let midnight_local = local.div_euclid(DAY_NS) * DAY_NS;
                let new_ns = midnight_local - off;
                Ok(Some(self.make_zoned_date_time_raw(new_ns, self.zdt_offset_ns(idx), idx)))
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
                    && self.tz_canon(idx) == self.tz_canon(oi);
                Ok(Some(Value::bool(eq)))
            }
            "withTimeZone" => {
                // Same instant, different zone. A wrong-type zone is a TypeError.
                let (id, offset) =
                    self.parse_tz_arg(args.first().copied().unwrap_or(Value::UNDEFINED))?;
                let ns = self.zdt_epoch_ns(idx).unwrap_or(0);
                Ok(Some(self.alloc_zdt(ns, offset, id)))
            }
            "withCalendar" => {
                // ISO 8601 only — accept "iso8601"/undefined/a calendar-bearing
                // Temporal, reject a wrong type (TypeError) or other calendar (RangeError).
                self.validate_calendar_value(args.first().copied().unwrap_or(Value::UNDEFINED))?;
                let (ns, off) = (self.zdt_epoch_ns(idx).unwrap_or(0), self.zdt_offset_ns(idx));
                Ok(Some(self.make_zoned_date_time_raw(ns, off, idx)))
            }
            "withPlainTime" => {
                let tv = args.first().copied().unwrap_or(Value::UNDEFINED);
                let time = if tv == Value::UNDEFINED {
                    [0i64; 6]
                } else {
                    self.to_plain_time(tv)?
                };
                let f = self.zdt_local(idx);
                let off = self.zdt_offset_ns(idx);
                let local = (iso_to_epoch_days(f[0], f[1], f[2]) as i128) * DAY_NS + time_to_ns(&time);
                let id = self.zdt_tz_id(idx).unwrap_or_else(|| "UTC".to_string());
                Ok(Some(self.alloc_zdt(local - off as i128, off, id)))
            }
            "add" | "subtract" => {
                // Fixed-offset zones: apply the same calendar/clock arithmetic as
                // PlainDateTime to the local wall-clock, then re-zone. (Named-zone
                // DST disambiguation is not modelled.)
                let dur = self.to_duration(args.first().copied().unwrap_or(Value::UNDEFINED))?;
                let reject = self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                let sign: i64 = if name == "add" { 1 } else { -1 };
                let lf = self.zdt_local(idx);
                let time = [lf[3], lf[4], lf[5], lf[6], lf[7], lf[8]];
                let tns = time_to_ns(&time)
                    + ((dur[4] as i128) * 3_600_000_000_000
                        + (dur[5] as i128) * 60_000_000_000
                        + (dur[6] as i128) * 1_000_000_000
                        + (dur[7] as i128) * 1_000_000
                        + (dur[8] as i128) * 1_000
                        + (dur[9] as i128))
                        * sign as i128;
                let carry = tns.div_euclid(DAY_NS) as i64;
                let nt = ns_to_time(tns.rem_euclid(DAY_NS));
                let tm = (lf[0] + dur[0] * sign) * 12 + (lf[1] - 1) + dur[1] * sign;
                let ny0 = tm.div_euclid(12);
                let nmo = tm.rem_euclid(12) + 1;
                if reject && lf[2] > days_in_month(ny0, nmo) {
                    return Err(Thrown("RangeError: date arithmetic overflows the month".into()));
                }
                let nd0 = lf[2].min(days_in_month(ny0, nmo));
                let ed = iso_to_epoch_days(ny0, nmo, nd0) + (dur[2] * 7 + dur[3]) * sign + carry;
                let off = self.zdt_offset_ns(idx);
                let local = (ed as i128) * DAY_NS + time_to_ns(&nt);
                let id = self.zdt_tz_id(idx).unwrap_or_else(|| "UTC".to_string());
                let result_ns = local - off as i128;
                // IsValidEpochNanoseconds: the result must lie within the supported
                // instant range (matches the ZonedDateTime/Instant constructors).
                if result_ns.abs() > NS_MAX_INSTANT {
                    return Err(Thrown(
                        "RangeError: ZonedDateTime result is outside the supported range".into(),
                    ));
                }
                Ok(Some(self.alloc_zdt(result_ns, off, id)))
            }
            "until" | "since" => {
                // Difference of two ZonedDateTimes (fixed-offset): the difference of
                // their local wall-clocks. Default largestUnit is "hour".
                let other = args.first().copied().unwrap_or(Value::UNDEFINED);
                let oz = self.zoned_date_time_from(other, Value::UNDEFINED)?;
                let of = self.zdt_local(oz.heap_index());
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
                let f = self.zdt_local(idx);
                // since = negate(until): forward (this → other) difference with a
                // sign-negated rounding mode, then negate the result.
                let (dt1, dt2) = (f, of);
                let eff = if name == "since" { negate_mode(&mode) } else { mode.clone() };
                let df = difference_datetime(dt1, dt2, &largest);
                let mut out = if rank(&largest) >= rank("day") {
                    let total_ns = (df[3] as i128) * DAY_NS
                        + time_to_ns(&[df[4], df[5], df[6], df[7], df[8], df[9]]);
                    let inc_ns = unit_ns(&smallest) * inc;
                    let rounded = round_increment(total_ns, inc_ns, &eff);
                    balance_duration_ns(rounded, &largest)
                } else if matches!(smallest.as_str(), "year" | "month" | "week") {
                    round_relative_datetime_diff(dt1, dt2, &smallest, &largest, inc, &eff)?
                } else {
                    round_datetime_diff_daytime(dt1, df, &smallest, &largest, inc, &eff)
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
                let time_ns = time_to_ns(&[f[3], f[4], f[5], f[6], f[7], f[8]]);
                let inc_ns = unit_ns(&su) * inc;
                let rounded = round_increment(time_ns, inc_ns, &mode);
                let day_carry = rounded.div_euclid(DAY_NS) as i64;
                let nt = ns_to_time(rounded.rem_euclid(DAY_NS));
                let ed = iso_to_epoch_days(f[0], f[1], f[2]) + day_carry;
                let off = self.zdt_offset_ns(idx);
                let local = (ed as i128) * DAY_NS + time_to_ns(&nt);
                let id = self.zdt_tz_id(idx).unwrap_or_else(|| "UTC".to_string());
                Ok(Some(self.alloc_zdt(local - off as i128, off, id)))
            }
            "with" => {
                // Merge date/time fields from the bag over the current local
                // wall-clock; the zone (and thus offset) is unchanged.
                let bag = args.first().copied().unwrap_or(Value::UNDEFINED);
                self.reject_temporal_like(bag)?;
                let mut f = self.zdt_local(idx);
                let names = [
                    "year", "month", "day", "hour", "minute", "second", "millisecond",
                    "microsecond", "nanosecond",
                ];
                let mut month_valid = true;
                let mut any = false;
                for (i, nm) in names.iter().enumerate() {
                    // The month slot goes through read_month_field_raw so monthCode is
                    // honoured, month/monthCode agreement is enforced, and a calendar-
                    // invalid code is deferred until after the options bag is read.
                    let v = if i == 1 {
                        self.read_month_field_raw(bag)?.map(|(mm, valid)| {
                            month_valid = valid;
                            mm
                        })
                    } else {
                        self.opt_int_field(bag, nm)?
                    };
                    if let Some(x) = v {
                        f[i] = x;
                        any = true;
                    }
                }
                // Read and validate the bag's `offset` field (a bad string is a
                // RangeError, a non-string a TypeError); its presence also satisfies the
                // "at least one recognized property" requirement.
                let bag_off = self.validate_bag_offset_field(bag)?;
                if !any && bag_off.is_none() {
                    return Err(Thrown(
                        "TypeError: with() requires at least one recognized property".into(),
                    ));
                }
                // month/day use ToPositiveIntegerWithTruncation: a value below 1 is
                // rejected during field preparation, BEFORE the options bag is read.
                if f[1] < 1 || f[2] < 1 {
                    return Err(Thrown("RangeError: invalid date fields".into()));
                }
                // Validate the resolution options. ZonedDateTime.with defaults the offset
                // option to "prefer" (unlike `from`, which defaults to "reject").
                let options = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let (off_opt, reject) = self.read_zdt_options(options, "prefer")?;
                // A well-formed-but-calendar-invalid monthCode ("M08L", "M13") is
                // rejected only after the options bag has been read.
                if !month_valid {
                    return Err(Thrown(
                        "RangeError: monthCode is not valid for the ISO 8601 calendar".into(),
                    ));
                }
                // InterpretTemporalDateTimeFields: apply overflow to the upper bounds of
                // the merged date/time fields ("reject" throws, "constrain" clamps).
                let maxes = [23, 59, 59, 999, 999, 999];
                if reject {
                    if !(1..=12).contains(&f[1]) || f[2] > days_in_month(f[0], f[1]) {
                        return Err(Thrown("RangeError: invalid date fields".into()));
                    }
                    for (i, &mx) in maxes.iter().enumerate() {
                        if f[3 + i] < 0 || f[3 + i] > mx {
                            return Err(Thrown("RangeError: time field out of range".into()));
                        }
                    }
                } else {
                    f[1] = f[1].min(12);
                    f[2] = f[2].min(days_in_month(f[0], f[1]));
                    for (i, &mx) in maxes.iter().enumerate() {
                        f[3 + i] = f[3 + i].clamp(0, mx);
                    }
                }
                // Offset agreement (InterpretISODateTimeOffset): the merged offset is the
                // bag's (when given) else the receiver's, which for zipp's fixed-offset
                // zones equals the zone offset. "use" keeps the merged offset; "ignore"/
                // "prefer" use the zone offset; "reject" requires the two to match.
                let zone_off = self.zdt_offset_ns(idx);
                let merged_off = bag_off.unwrap_or(zone_off);
                let eff = match off_opt.as_str() {
                    "use" => merged_off,
                    "ignore" | "prefer" => zone_off,
                    _ => {
                        if merged_off == zone_off {
                            zone_off
                        } else {
                            return Err(Thrown(
                                "RangeError: the offset does not match the time zone".into(),
                            ));
                        }
                    }
                };
                let local = (iso_to_epoch_days(f[0], f[1], f[2]) as i128) * DAY_NS
                    + time_to_ns(&[f[3], f[4], f[5], f[6], f[7], f[8]]);
                // The resulting instant must be representable (the ±nsMaxInstant bound).
                let instant = local - eff as i128;
                if instant.abs() > NS_MAX_INSTANT {
                    return Err(Thrown(
                        "RangeError: ZonedDateTime outside the supported range".into(),
                    ));
                }
                let id = self.zdt_tz_id(idx).unwrap_or_else(|| "UTC".to_string());
                Ok(Some(self.alloc_zdt(instant, zone_off, id)))
            }
            _ => Ok(None),
        }
    }

    /// Build a ZonedDateTime from epoch ns + offset, copying the time-zone id of an
    /// existing instance `src` (used by methods that derive a new ZDT in place).
    pub(crate) fn make_zoned_date_time_raw(&mut self, ns: i128, offset_ns: i64, src: u32) -> Value {
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
        Value::heap(idx)
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
                return Ok(self.make_zoned_date_time_raw(ns, off, item.heap_index()));
            }
            if matches!(self.heap.get(item.heap_index()), HeapObj::Object(_)) {
                // The calendar field is resolved (and validated) before the timeZone
                // requirement, so an invalid calendar is a RangeError even when the
                // timeZone is absent.
                self.validate_iso_calendar_field(item)?;
                let tzv = self.get_prop(item, "timeZone")?;
                if tzv == Value::UNDEFINED {
                    return Err(Thrown(
                        "TypeError: Temporal.ZonedDateTime.from requires a timeZone property".into(),
                    ));
                }
                // ToTemporalTimeZoneIdentifier: a string is parsed, a wrong type
                // (null/boolean/number/bigint/symbol) is a TypeError — not coerced.
                let (id, offset) = self.parse_tz_arg(tzv)?;
                let bag_off = self.validate_bag_offset_field(item)?;
                let (off_opt, reject) = self.read_zdt_options(options, "reject")?;
                let f = self.to_plain_date_time_overflow(item, reject)?;
                let local = (iso_to_epoch_days(f[0], f[1], f[2]) as i128) * DAY_NS
                    + time_to_ns(&[f[3], f[4], f[5], f[6], f[7], f[8]]);
                // Offset agreement: a bag `offset` is reconciled with the zone's offset
                // per the `offset` option. zipp's zones carry a single fixed offset, so:
                // reject → must equal it (else RangeError); use → use the bag offset for
                // the instant; ignore/prefer → use the zone offset.
                let eff = match bag_off {
                    None => offset,
                    Some(b) => match off_opt.as_str() {
                        "use" => b,
                        "ignore" | "prefer" => offset,
                        _ => {
                            if b != offset {
                                return Err(Thrown(
                                    "RangeError: offset does not match the time zone".into(),
                                ));
                            }
                            offset
                        }
                    },
                };
                return Ok(self.alloc_zdt(local - eff as i128, offset, id));
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
        let (f, str_offset, id, zone_offset, behaviour) = parse_zdt_string(&s)
            .ok_or_else(|| Thrown(format!("RangeError: invalid ZonedDateTime string \"{s}\"")))?;
        let (off_opt, _reject) = self.read_zdt_options(options, "reject")?;
        let local = (iso_to_epoch_days(f[0], f[1], f[2]) as i128) * DAY_NS
            + time_to_ns(&[f[3], f[4], f[5], f[6], f[7], f[8]]);
        // Offset agreement (InterpretISODateTimeOffset): a `Z` designator (EXACT) fixes
        // the instant as UTC and is never reconciled; no explicit offset (WALL) uses the
        // zone; an explicit offset (OPTION) that differs from the zone's is reconciled
        // per the `offset` option (reject=default → RangeError; use → the string offset
        // sets the instant; ignore/prefer → the zone offset).
        let eff = if behaviour == 1 {
            str_offset
        } else if str_offset == zone_offset {
            zone_offset
        } else {
            match off_opt.as_str() {
                "use" => str_offset,
                "ignore" | "prefer" => zone_offset,
                _ => {
                    return Err(Thrown(
                        "RangeError: the offset does not match the time zone".into(),
                    ))
                }
            }
        };
        Ok(self.alloc_zdt(local - eff as i128, zone_offset, id))
    }

    /// Allocate a ZonedDateTime from epoch ns, offset, and an (owned) tz id.
    pub(crate) fn alloc_zdt(&mut self, ns: i128, offset_ns: i64, id: String) -> Value {
        let hi = (ns >> 64) as i64;
        let lo = ns as i64;
        let idx = self.heap.alloc(HeapObj::Temporal { kind: 7, fields: vec![hi, lo, offset_ns] });
        if self.zoneddatetime_proto != 0 {
            self.proto_of.insert(idx, Value::heap(self.zoneddatetime_proto));
        }
        let idv = self.alloc_str(id);
        self.zdt_tz.insert(idx, idv);
        Value::heap(idx)
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

    /// Parse a `relativeTo` option into a date-time [y,mo,d,h,…] anchor (a
    /// ZonedDateTime uses its local wall-clock; otherwise PlainDate/PlainDateTime/
    /// string/object coercion).
    /// Resolve a `relativeTo` option to its anchor wall-clock fields plus a flag for
    /// whether it is ZonedDateTime-like (a ZDT instance, a `[tz]`-annotated string, or
    /// a bag carrying a `timeZone`). The flag selects the tighter ±nsMaxInstant epoch
    /// bound (vs the PlainDateTime ±(nsMaxInstant+nsPerDay) bound) for range checks.
    pub(crate) fn relative_to_dt(&mut self, rel: Value) -> Result<([i64; 9], bool), Thrown> {
        let mut is_zoned = false;
        if rel.is_heap() {
            if matches!(self.heap.get(rel.heap_index()), HeapObj::Temporal { kind: 7, .. }) {
                return Ok((self.zdt_local(rel.heap_index()), true));
            }
            // A property bag carrying a `timeZone` is a ZonedDateTime-like: the
            // time zone is validated (a non-string/non-object is a TypeError, an
            // invalid string a RangeError), then the wall-clock date/time is the
            // anchor. (A plain string relativeTo isn't an object, so it skips this.)
            if self.is_object_value(rel) {
                let tz = self.get_prop(rel, "timeZone")?;
                if tz != Value::UNDEFINED {
                    self.parse_tz_arg(tz)?;
                    self.validate_bag_offset_field(rel)?;
                    is_zoned = true;
                }
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
                let (f, str_offset, _id, zone_offset, behaviour) = parse_zdt_string(st)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid datetime string '{s}'")))?;
                if behaviour == 2 && str_offset != zone_offset {
                    return Err(Thrown(format!(
                        "RangeError: the relativeTo offset does not match the time zone in '{s}'"
                    )));
                }
                return Ok((f, true));
            }
            let f = parse_iso_datetime(main)
                .ok_or_else(|| Thrown(format!("RangeError: invalid datetime string '{s}'")))?;
            return Ok((f, false));
        }
        Ok((self.to_plain_date_time(rel)?, is_zoned))
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
        fa: [i64; 10],
        fb: [i64; 10],
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
        if let Some((start, zoned)) = start {
            // Both anchored end-points must be representable.
            check_relative_target(start, fa, zoned)?;
            check_relative_target(start, fb, zoned)?;
            let e1 = dt_epoch_ns(dt_add_dur(start, fa));
            let e2 = dt_epoch_ns(dt_add_dur(start, fb));
            return Ok(order(e1, e2));
        }
        if fa[..3].iter().any(|&x| x != 0) || fb[..3].iter().any(|&x| x != 0) {
            return Err(Thrown(
                "RangeError: a relativeTo option is required for years, months, or weeks".into(),
            ));
        }
        let tot = |f: &[i64; 10]| -> i128 {
            (f[3] as i128) * DAY_NS
                + time_to_ns(&[f[4], f[5], f[6], f[7], f[8], f[9]])
        };
        Ok(order(tot(&fa), tot(&fb)))
    }

    /// `Duration.round` with a relativeTo anchor: round the span `start →
    /// start+duration` exactly like `PlainDateTime.prototype.until`, so the
    /// calendar-unit nudging, the day/time remainder rounding (time-of-day included
    /// via epoch nanoseconds), and the re-balance to largestUnit are all shared.
    pub(crate) fn round_duration_relative(
        &mut self,
        f: [i64; 10],
        start: [i64; 9],
        smallest: &str,
        largest: &str,
        inc: i128,
        mode: &str,
    ) -> Result<[i64; 10], Thrown> {
        let end = dt_add_dur(start, f);
        let order = [
            "year", "month", "week", "day", "hour", "minute", "second", "millisecond",
            "microsecond", "nanosecond",
        ];
        let rank = |u: &str| order.iter().position(|&x| x == u).unwrap_or(9);
        if rank(largest) >= rank("day") {
            // A day-or-time largestUnit is a pure nanosecond span: round it, balance.
            let total_ns = dt_epoch_ns(end) - dt_epoch_ns(start);
            let rounded = round_increment(total_ns, unit_ns(smallest) * inc, mode);
            Ok(balance_duration_ns(rounded, largest))
        } else if matches!(smallest, "year" | "month" | "week") {
            // Calendar largestUnit + calendar smallestUnit → NudgeToCalendarUnit.
            round_relative_datetime_diff(start, end, smallest, largest, inc, mode)
        } else {
            // Calendar largestUnit + day/time smallestUnit → round the day+time
            // remainder and roll an overflowing day up into the calendar units.
            let df = difference_datetime(start, end, largest);
            Ok(round_datetime_diff_daytime(start, df, smallest, largest, inc, mode))
        }
    }

    /// The time-zone id string of a ZDT instance (for equality).
    fn zdt_tz_id(&self, idx: u32) -> Option<String> {
        self.zdt_tz
            .get(&idx)
            .and_then(|v| self.heap.str_cow(v.heap_index()).map(|s| s.into_owned()))
    }

    /// A canonical time-zone key for equality: an offset zone collapses to its
    /// formatted offset (so "+00"/"+0000"/"+00:00" all match), a named zone keeps
    /// its id. Calendars are always iso8601 here, so no calendar term is needed.
    fn tz_canon(&self, idx: u32) -> String {
        let id = self.zdt_tz_id(idx).unwrap_or_else(|| "UTC".to_string());
        if id.starts_with(['+', '-']) {
            format_offset(self.zdt_offset_ns(idx))
        } else {
            id
        }
    }

    /// ISO string for a ZonedDateTime: `YYYY-MM-DDTHH:MM:SS<offset>[<tzid>]`.
    pub(crate) fn zdt_to_string(&self, idx: u32) -> String {
        let f = self.zdt_local(idx);
        let off = self.zdt_offset_ns(idx);
        let offset = format_offset(off);
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
        let (unit, digits, omit, mode) = self.time_precision(options)?;
        let cal_suf = self.calendar_name_suffix(options)?;
        let (show_offset, tzn) = if options == Value::UNDEFINED {
            (true, "auto".to_string())
        } else {
            let off_opt = self.opt_string(options, "offset", "auto", &["auto", "never"])?;
            let tzn =
                self.opt_string(options, "timeZoneName", "auto", &["auto", "never", "critical"])?;
            (off_opt != "never", tzn)
        };
        let off = self.zdt_offset_ns(idx);
        // Round the instant to the requested unit, then express in the offset.
        let epoch = self.zdt_epoch_ns(idx).unwrap_or(0);
        let rounded = round_increment(epoch, unit, &mode);
        let local = rounded + off as i128;
        let t = ns_to_time(local.rem_euclid(DAY_NS));
        let (ny, nm, nd) = epoch_days_to_iso(local.div_euclid(DAY_NS) as i64);
        let offset_s = if show_offset { format_offset(off) } else { String::new() };
        let tz = self.zdt_tz_id(idx).unwrap_or_else(|| "UTC".to_string());
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

    fn parse_instant_string(&mut self, s: &str) -> Result<i128, Thrown> {
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
                let (unit, digits, omit, mode) = self.time_precision(a0)?;
                // The `timeZone` option: undefined -> UTC shown as "Z"; otherwise the
                // instant is expressed in that zone and the numeric offset is shown.
                let tz_v = if self.is_object_value(a0) {
                    self.get_prop(a0, "timeZone")?
                } else {
                    Value::UNDEFINED
                };
                let (offset, tz_str) = if tz_v == Value::UNDEFINED {
                    (0i64, "Z".to_string())
                } else {
                    let (_, off) = self.parse_tz_arg(tz_v)?;
                    (off, format_offset(off))
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
                Ok(Some(self.alloc_zdt(ns, offset, id)))
            }
            "add" | "subtract" => {
                let dur = self.to_duration(a0)?;
                if dur[0] != 0 || dur[1] != 0 || dur[2] != 0 || dur[3] != 0 {
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
                Ok(Some(self.make_duration(balance_duration_ns(rounded, &largest))))
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

    pub(crate) fn make_plain_year_month(&mut self, y: i64, m: i64, ref_day: i64) -> Result<Value, Thrown> {
        if !(1..=12).contains(&m) || !iso_year_month_in_range(y, m) {
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
        self.to_plain_year_month_overflow(v, false)
    }

    pub(crate) fn to_plain_year_month_overflow(
        &mut self,
        v: Value,
        reject: bool,
    ) -> Result<(i64, i64, i64), Thrown> {
        if v.is_heap() {
            if let Some(t) = self.pym_fields(v.heap_index()) {
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
                let yv = self.get_prop(v, "year")?;
                let m = self.read_month_field(v)?;
                if yv == Value::UNDEFINED || m.is_none() {
                    return Err(Thrown(
                        "TypeError: PlainYearMonth-like requires year and month".into(),
                    ));
                }
                // ToIntegerWithTruncation: reject a non-finite (NaN/±Infinity) year
                // and run the observable ToPrimitive (not the non-`&mut` to_number).
                let y = self.temporal_ctor_int(yv)?;
                let mut m = m.unwrap();
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
                // smallestUnit default "month"; largestUnit default "auto" → "year".
                let smallest =
                    normalize_unit(&self.opt_string(opts, "smallestUnit", "month", ym_units)?, "month");
                let largest_raw =
                    normalize_unit(&self.opt_string(opts, "largestUnit", "auto", ym_units)?, "auto");
                let largest = if largest_raw == "auto" { "year".to_string() } else { largest_raw };
                let rank = |u: &str| if u == "year" { 0 } else { 1 };
                if rank(&smallest) < rank(&largest) {
                    return Err(Thrown(
                        "RangeError: smallestUnit is larger than largestUnit".into(),
                    ));
                }
                let inc = self.read_rounding_increment(opts)?;
                let mode = self.read_rounding_mode(opts, "trunc")?;
                let from = y * 12 + (m - 1);
                let to = o.0 * 12 + (o.1 - 1);
                let total_months = if name == "until" { to - from } else { from - to };
                // Round to smallestUnit: whole years round to a multiple of 12·inc.
                let step = if smallest == "year" { 12 * inc } else { inc };
                let rounded = round_increment(total_months as i128, step, &mode) as i64;
                let mut f = [0i64; 10];
                if largest == "year" {
                    f[0] = rounded / 12;
                    f[1] = rounded % 12;
                } else {
                    f[1] = rounded;
                }
                Ok(Some(self.make_duration(f)))
            }
            "toPlainDate" => {
                let day = self.opt_int_field(a0, "day")?.ok_or_else(|| {
                    Thrown("TypeError: toPlainDate requires a day".into())
                })?;
                Ok(Some(self.make_plain_date(y, m, day)?))
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
        if !(1..=12).contains(&m) || d < 1 || d > days_in_month(ref_year, m) {
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
        self.to_plain_month_day_overflow(v, false)
    }

    pub(crate) fn to_plain_month_day_overflow(
        &mut self,
        v: Value,
        reject: bool,
    ) -> Result<(i64, i64, i64), Thrown> {
        if v.is_heap() {
            if let Some(t) = self.pmd_fields(v.heap_index()) {
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
                let m = self.read_month_field(v)?;
                let d_opt = self.opt_int_field(v, "day")?;
                // The reference `year` field is read (and finite-checked, rejecting
                // ±Infinity/NaN) even though this ISO engine always stores 1972 as
                // the reference ISO year.
                let _year = self.opt_int_field(v, "year")?;
                if m.is_none() || d_opt.is_none() {
                    return Err(Thrown(
                        "TypeError: PlainMonthDay-like requires month and day".into(),
                    ));
                }
                let mut m = m.unwrap();
                let mut d = d_opt.unwrap();
                if reject {
                    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(1972, m) {
                        return Err(Thrown("RangeError: month-day out of range".into()));
                    }
                } else {
                    // "constrain" clamps only the upper bound; month/day below 1 rejects.
                    if m < 1 || d < 1 {
                        return Err(Thrown("RangeError: month-day out of range".into()));
                    }
                    m = m.min(12);
                    d = d.min(days_in_month(1972, m));
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
                Ok(Some(self.make_plain_date(year, m, d)?))
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

/// `IsValidDuration` (spec): all fields finite, |years|/|months|/|weeks| < 2^32,
/// and the combined days+time span is under 2^53 seconds. Operates on the raw
/// f64 fields so out-of-range magnitudes are caught before any i64 truncation.
fn is_valid_duration(f: &[f64; 10]) -> bool {
    if f.iter().any(|v| !v.is_finite()) {
        return false;
    }
    let two_pow_32 = 4_294_967_296.0_f64; // 2^32
    if f[0].abs() >= two_pow_32 || f[1].abs() >= two_pow_32 || f[2].abs() >= two_pow_32 {
        return false;
    }
    // The bound is abs(days×86400 + h×3600 + m×60 + s + sub-seconds) < 2^53 s.
    // An f64 estimate decides everything except a thin band around 2^53 where
    // rounding is ambiguous (e.g. the spec maximum 2^53-1 + 0.999999999 rounds
    // up to 2^53 in f64); there, recompute the total exactly in i128 ns.
    let two53 = 9_007_199_254_740_992.0_f64; // 2^53
    let est = f[3] * 86_400.0
        + f[4] * 3_600.0
        + f[5] * 60.0
        + f[6]
        + f[7] / 1e3
        + f[8] / 1e6
        + f[9] / 1e9;
    // Margin must exceed the f64 ULP at 2^53 (which is 2.0, so 1.0 would vanish).
    if est.abs() >= two53 + 16.0 {
        return false;
    }
    if est.abs() <= two53 - 16.0 {
        return true;
    }
    // Ambiguous band. Exact i128 nanosecond total; bail out (reject) if any field
    // is beyond i64 range, which here can only arise from sign-cancellation (an
    // invalid mixed-sign duration) and would overflow the i128 products anyway.
    if f[3..10].iter().any(|x| x.abs() >= 9.0e18) {
        return false;
    }
    let total_ns: i128 = (f[3] as i128) * 86_400_000_000_000
        + (f[4] as i128) * 3_600_000_000_000
        + (f[5] as i128) * 60_000_000_000
        + (f[6] as i128) * 1_000_000_000
        + (f[7] as i128) * 1_000_000
        + (f[8] as i128) * 1_000
        + (f[9] as i128);
    total_ns.unsigned_abs() < 9_007_199_254_740_992u128 * 1_000_000_000
}

/// Validate the `[...]` annotation suffix of a Temporal ISO string per the
/// grammar's critical-flag rules: a critical annotation with an unknown key
/// (anything but `u-ca`) is rejected, at most one time-zone annotation is
/// allowed, and 2+ calendar (`u-ca`) annotations are rejected if any is
/// critical. `ann` starts at the first `[`.
fn annotations_valid(ann: &str) -> bool {
    let mut s = ann;
    let mut cal_count = 0u32;
    let mut cal_critical = false;
    let mut tz_count = 0u32;
    while !s.is_empty() {
        if !s.starts_with('[') {
            return false;
        }
        let end = match s.find(']') {
            Some(e) => e,
            None => return false,
        };
        let content = &s[1..end];
        s = &s[end + 1..];
        let (critical, body) = match content.strip_prefix('!') {
            Some(b) => (true, b),
            None => (false, content),
        };
        if let Some(eq) = body.find('=') {
            let key = &body[..eq];
            // AnnotationKey grammar: (a-z | _) then (a-z | _ | 0-9 | -)*. An
            // upper-cased or otherwise malformed key (e.g. "U-CA", "FOO") is a
            // syntax error — NOT an ignorable unknown annotation.
            let key_char = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_';
            let valid_key = key.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
                && key.bytes().all(key_char);
            if !valid_key {
                return false;
            }
            if key == "u-ca" {
                cal_count += 1;
                cal_critical |= critical;
            } else if critical {
                return false; // critical unknown key annotation
            }
        } else {
            // [Area/Location] or [±HH:MM] offset time-zone annotation. An offset-form
            // annotation must be minute precision (±HH, ±HH:MM, ±HHMM) — a sub-minute
            // one ([-07:00:01] / [-070001] / [-07:00:00.1]) is invalid.
            if let Some(off) = body.strip_prefix(['+', '-']) {
                let sub_minute = off.contains('.')
                    || off.contains(',')
                    || off.matches(':').count() >= 2
                    || (!off.contains(':') && off.bytes().filter(|c| c.is_ascii_digit()).count() > 4);
                if sub_minute {
                    return false;
                }
            }
            tz_count += 1;
        }
    }
    tz_count <= 1 && !(cal_count > 1 && cal_critical)
}

/// Validate a Temporal ISO string for a given parser context: the annotation
/// suffix must be well-formed, and (for the wall-clock "Plain" types) the string
/// must not carry a `Z`/`z` UTC designator (a numeric offset is still allowed).
/// `require_iso_calendar` (for the calendar-BEARING types — PlainDate/DateTime/
/// YearMonth/MonthDay/ZonedDateTime/Duration-relativeTo) additionally requires the
/// FIRST `[u-ca=…]` calendar annotation, if present, to be the supported "iso8601"
/// (so "…[u-ca=notacal]" / a date-like calendar name is a RangeError). The
/// calendar-LESS types (Instant, PlainTime) ignore the calendar entirely.
fn temporal_string_ok(s: &str, reject_utc_designator: bool, require_iso_calendar: bool) -> bool {
    let s = s.trim();
    let (main, ann) = match s.find('[') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    if !ann.is_empty() && !annotations_valid(ann) {
        return false;
    }
    // The first calendar annotation is the resolved calendar; later `[u-ca=…]` are
    // ignored. This ISO-only engine accepts only "iso8601".
    if require_iso_calendar {
        if let Some(p) = ann.find("u-ca=") {
            let val = &ann[p + 5..];
            match val.find(']') {
                Some(end) if val[..end].eq_ignore_ascii_case("iso8601") => {}
                _ => return false,
            }
        }
    }
    if reject_utc_designator && main.bytes().any(|b| b == b'Z' || b == b'z') {
        return false;
    }
    // No fractional run (sub-second OR a sub-minute offset) may exceed 9 digits.
    let chars: Vec<char> = main.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '.' || c == ',' {
            let n = chars[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
            if n > 9 {
                return false;
            }
        }
    }
    true
}

/// Resolve a calendar string to its canonical id. The ISO-only engine accepts
/// only "iso8601", but parsing the id lets an unsupported calendar error cleanly:
/// "iso8601" in any ASCII case, a `[u-ca=…]` annotation embedded in an ISO
/// string, or a bare ISO date / year-month / month-day string (→ iso8601).
fn calendar_id_from_string(s: &str) -> Option<String> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("iso8601") {
        return Some("iso8601".to_string());
    }
    if let Some(p) = s.find("u-ca=") {
        let val = &s[p + 5..];
        let end = val.find(']')?;
        return Some(val[..end].to_string());
    }
    if parse_iso_datetime(s).is_some()
        || parse_iso_date(s).is_some()
        || parse_iso_year_month(s).is_some()
        || parse_iso_month_day(s).is_some()
        || parse_temporal_time(s).is_some()
    {
        return Some("iso8601".to_string());
    }
    None
}

/// The rounding mode for the negated frame, used to implement `since` as
/// `negate(until)`: ceil/floor and halfCeil/halfFloor swap (they are direction-
/// sensitive); trunc/expand/halfTrunc/halfExpand/halfEven are sign-symmetric.
fn negate_mode(m: &str) -> String {
    match m {
        "ceil" => "floor",
        "floor" => "ceil",
        "halfCeil" => "halfFloor",
        "halfFloor" => "halfCeil",
        other => other,
    }
    .to_string()
}

/// Whether `s` is a well-formed UTC-offset string for a Temporal property-bag
/// `offset` field: a required sign, a 2-digit hour 00-23, then optional
/// minute / second components with a CONSISTENT separator style (all `:` or all
/// none), and an optional 1-9 digit sub-minute fraction. (Rejects "00:00" — no
/// sign, "+0" — short hour, "-000:00" — long hour, "+00:0000" — inconsistent.)
pub(crate) fn valid_offset_string(s: &str) -> bool {
    let b = s.as_bytes();
    let n = b.len();
    let two = |i: usize| -> Option<u32> {
        if i + 2 <= n && b[i].is_ascii_digit() && b[i + 1].is_ascii_digit() {
            Some((b[i] - b'0') as u32 * 10 + (b[i + 1] - b'0') as u32)
        } else {
            None
        }
    };
    if n == 0 || (b[0] != b'+' && b[0] != b'-') {
        return false;
    }
    // Hour: exactly two digits, 00-23.
    match two(1) {
        Some(h) if h <= 23 => {}
        _ => return false,
    }
    let mut i = 3;
    if i == n {
        return true;
    }
    let extended = b[i] == b':';
    // Minutes.
    if extended {
        i += 1;
    }
    match two(i) {
        Some(m) if m <= 59 => i += 2,
        _ => return false,
    }
    if i == n {
        return true;
    }
    // Seconds — the separator style must match the minutes'.
    if extended {
        if i >= n || b[i] != b':' {
            return false;
        }
        i += 1;
    }
    match two(i) {
        Some(sec) if sec <= 59 => i += 2,
        _ => return false,
    }
    if i == n {
        return true;
    }
    // Optional 1-9 digit sub-second fraction terminating the string.
    if b[i] == b'.' || b[i] == b',' {
        let frac = &b[i + 1..];
        return !frac.is_empty() && frac.len() <= 9 && frac.iter().all(|c| c.is_ascii_digit());
    }
    false
}

/// Parse a Temporal time-zone argument into a (normalized id, offset-ns) pair.
/// Stage 1: "UTC" and numeric offsets (±HH:MM[:SS]) carry a real offset; a named
/// IANA-style id ("Area/Location") is accepted with offset 0 (no tz database yet).
fn parse_time_zone(s: &str) -> Option<(String, i64)> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    // A full ISO string carrying a `[tz]` annotation: the time zone is the bracket
    // content (e.g. "2016-12-31T23:59:60+00:00[UTC]" -> "UTC", "…[+01:46]" -> the
    // offset zone). A leading `!` critical flag is stripped.
    if let Some(lb) = t.find('[') {
        let rb = t[lb..].find(']').map(|r| lb + r)?;
        let inner = &t[lb + 1..rb];
        let inner = inner.strip_prefix('!').unwrap_or(inner);
        // A non-empty prefix before the annotation means the whole string is a
        // "<datetime|offset>[tz]" form — the prefix is NOT decorative and must be a
        // valid ISO datetime/offset. Validate it (e.g. reject
        // "-000000-10-31T17:45+00:00[UTC]" for its negative-zero extended year),
        // not just the bracket content. A pure UTC offset (only digits/`:`/`.`/`,`
        // after the sign) is checked directly; anything else must parse as a full
        // zoned-datetime string (which validates the date via the hardened parser).
        let prefix = t[..lb].trim();
        if !prefix.is_empty() {
            let pure_offset = prefix.strip_prefix(['+', '-']).is_some_and(|r| {
                !r.is_empty()
                    && r.chars().all(|c| c.is_ascii_digit() || matches!(c, ':' | '.' | ','))
            });
            if pure_offset {
                if parse_offset_ns(prefix).is_none() {
                    return None;
                }
            } else if parse_zdt_string(t).is_none() {
                return None;
            }
        }
        return parse_time_zone(inner);
    }
    if t.eq_ignore_ascii_case("UTC") {
        return Some(("UTC".to_string(), 0));
    }
    let b = t.as_bytes();
    if b[0] == b'+' || b[0] == b'-' {
        let sign: i64 = if b[0] == b'-' { -1 } else { 1 };
        let body = &t[1..];
        // Both colon-separated (±HH, ±HH:MM, ±HH:MM:SS) and colon-less (±HH,
        // ±HHMM, ±HHMMSS) offset forms are valid time-zone identifiers. A
        // sub-minute/fractional offset is NOT a valid identifier (rejected).
        let (hh, mm, ss) = if body.contains(':') {
            let parts: Vec<&str> = body.split(':').collect();
            if parts.len() > 3 {
                return None;
            }
            let hh: i64 = parts.first()?.parse().ok()?;
            let mm: i64 = parts.get(1).map_or(Some(0), |p| p.parse().ok())?;
            let ss: i64 = parts.get(2).map_or(Some(0), |p| p.parse().ok())?;
            (hh, mm, ss)
        } else {
            if !body.bytes().all(|c| c.is_ascii_digit()) || !matches!(body.len(), 2 | 4 | 6) {
                return None;
            }
            let hh: i64 = body[0..2].parse().ok()?;
            let mm: i64 = if body.len() >= 4 { body[2..4].parse().ok()? } else { 0 };
            let ss: i64 = if body.len() >= 6 { body[4..6].parse().ok()? } else { 0 };
            (hh, mm, ss)
        };
        if hh > 23 || mm > 59 || ss > 59 {
            return None;
        }
        let off = sign * (hh * 3600 + mm * 60 + ss) * 1_000_000_000;
        return Some((t.to_string(), off));
    }
    // A bracket-less full ISO datetime string carries its own zone: "...Z" → UTC,
    // "...±HH:MM"/"...±HHMM" (minute precision) → that offset zone, normalized via
    // format_offset. A sub-minute offset or a bare datetime (no offset/Z) is NOT a
    // time-zone identifier. (Bracketed forms returned at the `[` branch above; bare
    // offset ids hit the `+`/`-` branch above.)
    if let Some(sep) = t.find(['T', 't', ' ']) {
        if parse_iso_date(&t[..sep]).is_some() {
            let tp = &t[sep + 1..];
            if let Some(z) = tp.find(['Z', 'z']) {
                return parse_iso_time(&tp[..z]).map(|_| ("UTC".to_string(), 0));
            }
            if let Some(o) = tp.find(['+', '-']) {
                let (time_str, off_str) = (&tp[..o], &tp[o..]);
                let after = &off_str[1..];
                // Minute precision only: a second ':' (=seconds) or a fraction is a
                // sub-minute offset, which is not a valid identifier.
                let minute_prec = after.matches(':').count() <= 1
                    && !after.contains('.')
                    && !after.contains(',')
                    && after.bytes().all(|c| c.is_ascii_digit() || c == b':');
                if parse_iso_time(time_str).is_some() && minute_prec {
                    if let Some(off) = parse_offset_ns(off_str) {
                        let off = off as i64;
                        return Some((format_offset(off), off));
                    }
                }
            }
            return None;
        }
    }
    // A named zone like "America/New_York" or "Europe/London": accept the id.
    if t.contains('/') || t.chars().all(|c| c.is_ascii_alphabetic() || c == '_') {
        return Some((t.to_string(), 0));
    }
    None
}

/// Add a Duration `f` ([y,mo,w,d,h,mi,s,ms,us,ns]) to a date-time `start`
/// ([y,mo,d,h,mi,s,ms,us,ns]) with calendar constrain — the shared add path.
fn dt_add_dur(start: [i64; 9], f: [i64; 10]) -> [i64; 9] {
    let tns = time_to_ns(&[start[3], start[4], start[5], start[6], start[7], start[8]])
        + (f[4] as i128) * 3_600_000_000_000
        + (f[5] as i128) * 60_000_000_000
        + (f[6] as i128) * 1_000_000_000
        + (f[7] as i128) * 1_000_000
        + (f[8] as i128) * 1_000
        + (f[9] as i128);
    let carry = tns.div_euclid(DAY_NS) as i64;
    let nt = ns_to_time(tns.rem_euclid(DAY_NS));
    let tm = (start[0] + f[0]) * 12 + (start[1] - 1) + f[1];
    let ny0 = tm.div_euclid(12);
    let nmo = tm.rem_euclid(12) + 1;
    let nd0 = start[2].min(days_in_month(ny0, nmo));
    let ed = iso_to_epoch_days(ny0, nmo, nd0) + f[2] * 7 + f[3] + carry;
    let (ny, nm, nd) = epoch_days_to_iso(ed);
    [ny, nm, nd, nt[0], nt[1], nt[2], nt[3], nt[4], nt[5]]
}

/// Epoch nanoseconds of a date-time [y,mo,d,h,mi,s,ms,us,ns] (ISO/UTC frame).
fn dt_epoch_ns(dt: [i64; 9]) -> i128 {
    (iso_to_epoch_days(dt[0], dt[1], dt[2]) as i128) * DAY_NS
        + time_to_ns(&[dt[3], dt[4], dt[5], dt[6], dt[7], dt[8]])
}

/// Range-check the instant after adding duration `f` to a `relativeTo` anchor (the
/// target of Duration.prototype.total / round). The result must be representable:
/// a ZonedDateTime anchor uses the tight inclusive ±nsMaxInstant epoch bound (and
/// its own start is checked too — a string/bag ZDT anchor is not constructor-checked);
/// a Plain anchor uses ISODateTimeWithinLimits on the target only (its start is a
/// valid PlainDate/PlainDateTime by parse, possibly at the day-granular min boundary
/// that the nanosecond datetime bound would wrongly reject).
fn check_relative_target(start: [i64; 9], f: [i64; 10], is_zoned: bool) -> Result<(), Thrown> {
    let end = dt_add_dur(start, f);
    let ok = if is_zoned {
        // ZonedDateTime: the tight inclusive epoch bound (IsValidEpochNanoseconds).
        dt_epoch_ns(start).abs() <= NS_MAX_INSTANT && dt_epoch_ns(end).abs() <= NS_MAX_INSTANT
    } else {
        // Plain: the inclusive datetime bound (±(nsMaxInstant + nsPerDay)). Inclusive so
        // the day-granular min/max date anchors (e.g. -271821-04-19T00:00:00, exactly at
        // the boundary) are accepted, while genuine overflows still fail.
        dt_epoch_ns(end).abs() <= NS_MAX_INSTANT + DAY_NS
    };
    if !ok {
        return Err(Thrown("RangeError: Temporal result is outside the representable range".into()));
    }
    Ok(())
}

/// `nsMaxInstant` — the inclusive epoch-nanosecond bound of `Temporal.Instant`
/// (±8.64 × 10^21, i.e. ±10^8 days).
pub(crate) const NS_MAX_INSTANT: i128 = 8_640_000_000_000_000_000_000;

/// ISODateTimeWithinLimits: whether the date-time `f` is a representable
/// `Temporal.PlainDateTime` — its epoch-ns must lie strictly within `nsMaxInstant`
/// plus a one-day (`nsPerDay`) margin on each side. (Unlike `PlainDate`'s
/// day-granular bound, this is nanosecond-precise: `-271821-04-19T00:00:00.000000000`
/// is out of range but `…000000001` is in range.) Caller must have already
/// validated the field ranges so `dt_epoch_ns` cannot overflow.
pub(crate) fn iso_datetime_ns_in_range(f: [i64; 9]) -> bool {
    let ns = dt_epoch_ns(f);
    ns > -NS_MAX_INSTANT - DAY_NS && ns < NS_MAX_INSTANT + DAY_NS
}

/// Round a date-time difference `df` whose smallestUnit is a DAY-or-time unit while
/// largestUnit is a calendar unit (year/month/week): round the day+time remainder to
/// the smallestUnit increment, then re-balance to `largest` so a rounded-up day rolls
/// into the calendar units. The `rounded == time_ns` short-circuit keeps the
/// nanosecond-default case (and difference_datetime's day-borrow) byte-identical.
fn round_datetime_diff_daytime(
    dt1: [i64; 9],
    df: [i64; 10],
    smallest: &str,
    largest: &str,
    inc: i128,
    mode: &str,
) -> [i64; 10] {
    let time_ns = (df[3] as i128) * DAY_NS
        + time_to_ns(&[df[4], df[5], df[6], df[7], df[8], df[9]]);
    let rounded = round_increment(time_ns, unit_ns(smallest) * inc, mode);
    if rounded == time_ns {
        return df;
    }
    // Reconstruct the rounded endpoint (anchor at the kept year/month/week units, then
    // add the rounded day+time span) and re-decompose at largestUnit.
    let anchor = dt_add_dur(dt1, [df[0], df[1], df[2], 0, 0, 0, 0, 0, 0, 0]);
    let total = dt_epoch_ns(anchor) + rounded;
    let (ey, em, ed) = epoch_days_to_iso(total.div_euclid(DAY_NS) as i64);
    let t = ns_to_time(total.rem_euclid(DAY_NS));
    let end = [ey, em, ed, t[0], t[1], t[2], t[3], t[4], t[5]];
    difference_datetime(dt1, end, largest)
}

/// Round the date-time difference dt1→dt2 to a calendar `smallest` unit
/// (year/month/week), then balance to `largest`. Like round_relative_date_diff
/// but the fraction toward the next unit is measured in epoch NANOSECONDS, so
/// the time-of-day contributes (NudgeToCalendarUnit for PlainDateTime/ZDT).
fn round_relative_datetime_diff(
    dt1: [i64; 9],
    dt2: [i64; 9],
    smallest: &str,
    largest: &str,
    inc: i128,
    mode: &str,
) -> Result<[i64; 10], Thrown> {
    let si = ["year", "month", "week"].iter().position(|&x| x == smallest).unwrap_or(2);
    let ns1 = dt_epoch_ns(dt1);
    let ns2 = dt_epoch_ns(dt2);
    let sign = (ns2 > ns1) as i64 - (ns2 < ns1) as i64;
    if sign == 0 {
        return Ok([0; 10]);
    }
    // Decompose at largestUnit to KEEP the units above smallestUnit; only the
    // smallestUnit component is rounded (the sub-smallest remainder, including the
    // time-of-day, becomes the epoch-ns fraction toward the next increment).
    let base = difference_datetime(dt1, dt2, largest);
    // smallestUnit = week: difference dumps the sub-month remainder into days, so
    // derive the whole-week count from the full sub-week day span.
    let sval = if si == 2 { (base[2] * 7 + base[3]) / 7 } else { base[si] };
    let mk = |k: i64| -> [i64; 10] {
        let mut d = [0i64; 10];
        d[..si].copy_from_slice(&base[..si]);
        d[si] = k;
        d
    };
    let r1 = round_increment(sval as i128, inc, "trunc") as i64;
    let r2 = r1 + inc as i64 * sign;
    let lower = dt_add_dur(dt1, mk(r1));
    let ld = dt_epoch_ns(lower);
    // The r2 endpoint is a CalendarDateAdd(constrain) that must lie within the ISO
    // date limits — a huge increment can push it past the range (RangeError).
    let upper = dt_add_dur(dt1, mk(r2));
    if !iso_date_in_range(upper[0], upper[1], upper[2]) {
        return Err(Thrown(
            "RangeError: rounded date is outside the valid ISO range".into(),
        ));
    }
    let picked = if ld == ns2 {
        r1
    } else {
        let ud = dt_epoch_ns(upper);
        let progress = if ud != ld { (ns2 - ld) as f64 / (ud - ld) as f64 } else { 0.0 };
        round_fraction(r1 / inc as i64, sign, progress, mode) * inc as i64
    };
    // Weeks never fold into a larger calendar unit: keep years/months + rounded weeks.
    if si == 2 {
        return Ok([base[0], base[1], picked, 0, 0, 0, 0, 0, 0, 0]);
    }
    // Re-balance the kept-larger + rounded-smallest endpoint to largestUnit.
    let end = dt_add_dur(dt1, mk(picked));
    let d = difference_iso_date((dt1[0], dt1[1], dt1[2]), (end[0], end[1], end[2]), largest);
    let mut f = [0i64; 10];
    f[..4].copy_from_slice(&d);
    Ok(f)
}

/// `Duration.total(unit)` relative to a start date-time: the (possibly fractional)
/// total of the duration measured in `unit`, computed via the calendar at `start`.
fn duration_total_relative(f: [i64; 10], start: [i64; 9], unit: &str) -> Result<f64, Thrown> {
    let end_ns = dt_epoch_ns(dt_add_dur(start, f));
    let start_ns = dt_epoch_ns(start);
    let diff = end_ns - start_ns;
    match unit {
        "year" | "month" => {
            let sign = if diff < 0 { -1i64 } else { 1 };
            let units = |k: i64| -> [i64; 10] {
                let mut d = [0i64; 10];
                if unit == "year" {
                    d[0] = k;
                } else {
                    d[1] = k;
                }
                d
            };
            // Whole signed units, ALWAYS re-added from the anchor (chaining would let
            // day-of-month clamping accumulate and corrupt the unit length).
            let mut whole = 0i64;
            for _ in 0..2_000_000 {
                let cand = dt_epoch_ns(dt_add_dur(start, units(whole + sign)));
                if (sign > 0 && cand > end_ns) || (sign < 0 && cand < end_ns) {
                    break;
                }
                whole += sign;
            }
            // NudgeToCalendarUnit brackets the duration between `whole` and `whole+sign`
            // calendar units from the anchor (both via CalendarDateAdd). The FAR bracket
            // (past the duration's end) is the one that can exceed the date range; it
            // must be representable (inclusive ±(nsMaxInstant+nsPerDay), so the
            // day-granular min/max date boundary itself is still accepted).
            let far = dt_add_dur(start, units(whole + sign));
            if dt_epoch_ns(far).abs() > NS_MAX_INSTANT + DAY_NS {
                return Err(Thrown(
                    "RangeError: Temporal result is outside the representable range".into(),
                ));
            }
            // The fraction is the signed progress over the anchor-based unit length.
            let lower_ns = dt_epoch_ns(dt_add_dur(start, units(whole)));
            let upper_ns = dt_epoch_ns(far);
            if upper_ns != lower_ns {
                // whole + sign·(end-lower)/(upper-lower) as one correctly-rounded
                // rational, so the final addition can't round the wrong way.
                let span = upper_ns - lower_ns;
                Ok(rational_to_f64(
                    whole as i128 * span + sign as i128 * (end_ns - lower_ns),
                    span,
                ))
            } else {
                Ok(whole as f64)
            }
        }
        "week" => {
            // One correctly-rounded division of the exact total (the naive
            // whole+remainder split is still 1 ULP off in some cases).
            Ok(rational_to_f64(diff, 7 * DAY_NS))
        }
        _ => {
            // Same for a fixed-length unit (day/hour/…/ns).
            Ok(rational_to_f64(diff, unit_ns(unit)))
        }
    }
}

/// Parse a ZonedDateTime ISO string `<date>[T<time>][±OFF|Z][tzid][annotations]`
/// into (date-time fields, offset ns, tz id). The `[tzid]` annotation is REQUIRED
/// (it carries the zone); a leading `!` critical flag is stripped. The explicit
/// numeric offset / `Z` is OPTIONAL — when absent the offset comes from the zone
/// (so `1970-01-01T00:00[UTC]` and `2020-01-01[+09:00]` parse). The time part is
/// optional (date-only -> midnight). Basic-format offsets (`-0800`) are accepted.
/// Parse a ZonedDateTime/relativeTo string. Returns the wall-clock fields, the
/// instant offset (ns), the zone id, the zone's offset (ns), and the *offset
/// behaviour*: `0` = WALL (no explicit offset → use the zone), `1` = EXACT (a `Z`
/// designator → the instant is UTC, the offset is not reconciled), `2` = OPTION
/// (an explicit `±HH:MM` offset → reconcile against the zone per the `offset`
/// option). The behaviour drives ToTemporalZonedDateTime's InterpretISODateTimeOffset.
fn parse_zdt_string(s: &str) -> Option<([i64; 9], i64, String, i64, i8)> {
    let lb = s.find('[')?;
    let rb = s[lb..].find(']').map(|r| lb + r)?;
    let mut tz = s[lb + 1..rb].to_string();
    if let Some(stripped) = tz.strip_prefix('!') {
        tz = stripped.to_string();
    }
    // The bracket must be a valid time zone (named or numeric-offset); its offset
    // is the fallback when the datetime carries no explicit offset.
    let (tz_id, tz_offset) = parse_time_zone(&tz)?;
    let head = &s[..lb];
    // Split off an optional time part at the date/time separator.
    let (date_part, time_part) = match head.find(['T', 't', ' ']) {
        Some(tp) => (&head[..tp], Some(&head[tp + 1..])),
        None => (head, None),
    };
    let date = parse_iso_date(date_part)?;
    // In the time part, locate an explicit `Z` or numeric offset (else use the
    // zone's offset). The time itself never contains `+`/`-`, so the first one
    // begins the offset.
    let (time_str, offset_ns, behaviour) = match time_part {
        None => ("", tz_offset, 0i8),
        Some(t) => {
            if let Some(zpos) = t.find(['Z', 'z']) {
                (&t[..zpos], 0i64, 1i8)
            } else if let Some(opos) = t.find(['+', '-']) {
                (&t[..opos], parse_offset_ns(&t[opos..])? as i64, 2i8)
            } else {
                (t, tz_offset, 0i8)
            }
        }
    };
    let time = if time_str.is_empty() { [0i64; 6] } else { parse_iso_time(time_str)? };
    let f = [date.0, date.1, date.2, time[0], time[1], time[2], time[3], time[4], time[5]];
    // Return the zone's offset alongside the (possibly explicit) string offset so the
    // caller can reconcile a mismatch per the `offset` option / offset behaviour.
    Some((f, offset_ns, tz_id, tz_offset, behaviour))
}

/// Format a UTC offset (nanoseconds) as `±HH:MM` (or `±HH:MM:SS` when needed).
fn format_offset(ns: i64) -> String {
    let sign = if ns < 0 { '-' } else { '+' };
    let total = ns.abs() / 1_000_000_000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if s == 0 {
        format!("{sign}{h:02}:{m:02}")
    } else {
        format!("{sign}{h:02}:{m:02}:{s:02}")
    }
}
