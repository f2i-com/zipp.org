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
                // arg: a unit string, or { unit, relativeTo }.
                let unit_v = if a0.is_heap() && self.heap.is_str_like(a0.heap_index()) {
                    a0
                } else if a0 == Value::UNDEFINED {
                    return Err(Thrown("TypeError: total() requires an options argument".into()));
                } else if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: total() argument must be a string or object".into()));
                } else {
                    self.get_prop(a0, "unit")?
                };
                if unit_v == Value::UNDEFINED {
                    return Err(Thrown("RangeError: unit is required".into()));
                }
                let unit = normalize_unit(&self.to_js_string(unit_v)?, "");
                if !DURATION_UNITS.contains(&unit.as_str()) {
                    return Err(Thrown(format!("RangeError: invalid unit: {unit}")));
                }
                // Years/months/weeks (in the value or as the requested unit) need a
                // calendar: use the `relativeTo` option's date-time as the anchor.
                if f[0] != 0 || f[1] != 0 || f[2] != 0 || matches!(unit.as_str(), "year" | "month" | "week") {
                    let rel = if a0.is_heap() && !self.heap.is_str_like(a0.heap_index()) {
                        self.get_prop(a0, "relativeTo")?
                    } else {
                        Value::UNDEFINED
                    };
                    if rel == Value::UNDEFINED {
                        return Err(Thrown(
                            "RangeError: a relativeTo option is required for years, months, or weeks"
                                .into(),
                        ));
                    }
                    let start = self.relative_to_dt(rel)?;
                    return Ok(Some(Value::num(duration_total_relative(f, start, &unit))));
                }
                let total_ns = (f[3] as i128) * DAY_NS
                    + time_to_ns(&[f[4], f[5], f[6], f[7], f[8], f[9]]);
                Ok(Some(Value::num(total_ns as f64 / unit_ns(&unit) as f64)))
            }
            "round" => {
                let (su_v, options) = if a0.is_heap() && self.heap.is_str_like(a0.heap_index()) {
                    (a0, Value::UNDEFINED)
                } else if a0 == Value::UNDEFINED {
                    return Err(Thrown("TypeError: round() requires an options argument".into()));
                } else if !self.is_object_value(a0) {
                    return Err(Thrown("TypeError: round() argument must be a string or object".into()));
                } else {
                    (self.get_prop(a0, "smallestUnit")?, a0)
                };
                let lu_v = if options == Value::UNDEFINED {
                    Value::UNDEFINED
                } else {
                    self.get_prop(options, "largestUnit")?
                };
                let su = if su_v == Value::UNDEFINED {
                    None
                } else {
                    let s = normalize_unit(&self.to_js_string(su_v)?, "");
                    if !DURATION_UNITS.contains(&s.as_str()) {
                        return Err(Thrown(format!("RangeError: invalid smallestUnit: {s}")));
                    }
                    Some(s)
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
                if su.is_none() && lu.is_none() {
                    return Err(Thrown(
                        "RangeError: at least one of smallestUnit or largestUnit is required".into(),
                    ));
                }
                let inc = self.read_rounding_increment(options)?;
                let mode = if options == Value::UNDEFINED {
                    "halfExpand".to_string()
                } else {
                    self.read_rounding_mode(options, "halfExpand")?
                };
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
                // A relativeTo anchor enables calendar-unit rounding/balancing.
                let rel = if options == Value::UNDEFINED {
                    Value::UNDEFINED
                } else {
                    self.get_prop(options, "relativeTo")?
                };
                if rel != Value::UNDEFINED {
                    let start = self.relative_to_dt(rel)?;
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
                if let Some(max) = max_increment(&smallest) {
                    if inc >= max || max % inc != 0 {
                        return Err(Thrown(
                            "RangeError: roundingIncrement must evenly divide the next unit".into(),
                        ));
                    }
                }
                let total_ns = (f[3] as i128) * DAY_NS
                    + time_to_ns(&[f[4], f[5], f[6], f[7], f[8], f[9]]);
                let inc_ns = unit_ns(&smallest) * inc;
                let rounded = round_increment(total_ns, inc_ns, &mode);
                Ok(Some(self.make_duration(balance_duration_ns(rounded, &largest))))
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
                Ok(Some(self.make_duration(balance_duration_ns(total_ns, largest))))
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
    pub(crate) fn read_zdt_options(&mut self, options: Value) -> Result<bool, Thrown> {
        if options != Value::UNDEFINED && !self.is_object_value(options) {
            return Err(Thrown("TypeError: options must be an object or undefined".into()));
        }
        self.opt_string(
            options,
            "disambiguation",
            "compatible",
            &["compatible", "earlier", "later", "reject"],
        )?;
        self.opt_string(options, "offset", "reject", &["prefer", "use", "ignore", "reject"])?;
        self.read_overflow(options)
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
        let fsd_v = self.get_prop(options, "fractionalSecondDigits")?;
        if fsd_v == Value::UNDEFINED {
            return Ok((1, -1, false, mode));
        }
        // GetStringOrNumberOption dispatches on the RAW type, not coercibility: a
        // genuine Number is floored into 0..9; anything else (string/null/boolean/
        // bigint/object) is ToString'd and must equal exactly "auto" (a Symbol
        // ToString throws TypeError inside to_js_string).
        if !fsd_v.is_number() {
            if self.to_js_string(fsd_v)? == "auto" {
                return Ok((1, -1, false, mode));
            }
            return Err(Thrown("RangeError: fractionalSecondDigits must be 'auto' or 0..9".into()));
        }
        let n = self.to_number(fsd_v)?;
        if n.is_nan() {
            return Err(Thrown("RangeError: fractionalSecondDigits is NaN".into()));
        }
        let n = n.floor() as i64;
        if !(0..=9).contains(&n) {
            return Err(Thrown("RangeError: fractionalSecondDigits out of range".into()));
        }
        let unit = 10i128.pow(9 - n as u32);
        Ok((unit, n as i32, false, mode))
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
        mode: &str,
    ) -> [i64; 4] {
        let rank =
            |u: &str| ["year", "month", "week", "day"].iter().position(|&x| x == u).unwrap_or(3);
        let si = rank(smallest);
        let e1 = iso_to_epoch_days(d1.0, d1.1, d1.2);
        let e2 = iso_to_epoch_days(d2.0, d2.1, d2.2);
        let sign = (e2 > e1) as i64 - (e2 < e1) as i64;
        if sign == 0 {
            return [0, 0, 0, 0];
        }
        // Whole count of the smallest unit from d1 to d2.
        let count = difference_iso_date(d1, d2, smallest)[si];
        let mk = |k: i64| -> [i64; 10] {
            let mut dur = [0i64; 10];
            dur[si] = k;
            dur
        };
        let lower = self.date_add(d1.0, d1.1, d1.2, &mk(count), 1);
        let ld = iso_to_epoch_days(lower.0, lower.1, lower.2);
        let rounded = if ld == e2 {
            count
        } else {
            let upper = self.date_add(d1.0, d1.1, d1.2, &mk(count + sign), 1);
            let ud = iso_to_epoch_days(upper.0, upper.1, upper.2);
            let denom = (ud - ld) as f64;
            let progress = if denom != 0.0 { (e2 - ld) as f64 / denom } else { 0.0 };
            round_fraction(count, sign, progress, mode)
        };
        // Balance up to largestUnit (only months can fold into years).
        match si {
            1 if rank(largest) == 0 => {
                let end = self.date_add(d1.0, d1.1, d1.2, &mk(rounded), 1);
                difference_iso_date(d1, end, "year")
            }
            0 => [rounded, 0, 0, 0],
            1 => [0, rounded, 0, 0],
            _ => [0, 0, rounded, 0],
        }
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
            "toPlainYearMonth" => Ok(Some(self.make_plain_year_month(y, m, d)?)),
            "toPlainMonthDay" => Ok(Some(self.make_plain_month_day(m, d, y)?)),
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
                let reject = self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                let ny = self.opt_int_field(a0, "year")?.unwrap_or(y);
                let mut nm = self.opt_int_field(a0, "month")?.unwrap_or(m);
                let mut nd = self.opt_int_field(a0, "day")?.unwrap_or(d);
                if !reject {
                    nm = nm.clamp(1, 12);
                    nd = nd.clamp(1, days_in_month(ny, nm));
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
                // until: this → other; since: other → this.
                let (d1, d2) = if name == "until" {
                    ((y, m, d), other)
                } else {
                    (other, (y, m, d))
                };
                let mut f = [0i64; 10];
                // The day field rounds to the increment; a calendar smallestUnit
                // (year/month/week) rounds the fractional remainder against the
                // anchor calendar (NudgeToCalendarUnit) and balances to largestUnit.
                let si = rank(&smallest);
                if si == 3 {
                    let diff = difference_iso_date(d1, d2, &largest);
                    f[..4].copy_from_slice(&diff);
                    f[3] = round_increment(f[3] as i128, inc, &mode) as i64;
                } else {
                    let r = self.round_relative_date_diff(d1, d2, &smallest, &largest, &mode);
                    f[..4].copy_from_slice(&r);
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
        // `to_number_coerce` (not the non-`&mut` `to_number`) so an object field runs
        // the observable ToPrimitive (valueOf/@@toPrimitive) in spec order, and a
        // Symbol/BigInt is rejected with a TypeError.
        let n = self.to_number_coerce(v)?;
        if !n.is_finite() {
            return Err(Thrown(
                "RangeError: Temporal field must be a finite number".into(),
            ));
        }
        Ok(n.trunc() as i64)
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
                let reject = self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                let names =
                    ["hour", "minute", "second", "millisecond", "microsecond", "nanosecond"];
                let maxes = [23, 59, 59, 999, 999, 999];
                let mut nf = f;
                let mut any = false;
                for (i, nm) in names.iter().enumerate() {
                    if let Some(x) = self.opt_int_field(a0, nm)? {
                        nf[i] = if reject { x } else { x.clamp(0, maxes[i]) };
                        any = true;
                    }
                }
                if !any {
                    return Err(Thrown("TypeError: with() requires a partial time object".into()));
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
            "toPlainYearMonth" => Ok(Some(self.make_plain_year_month(date[0], date[1], date[2])?)),
            "toPlainMonthDay" => Ok(Some(self.make_plain_month_day(date[1], date[2], date[0])?)),
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
                let (id, offset) = self.parse_tz_arg(a0)?;
                let local = (iso_to_epoch_days(f[0], f[1], f[2]) as i128) * DAY_NS
                    + time_to_ns(&[f[3], f[4], f[5], f[6], f[7], f[8]]);
                Ok(Some(self.alloc_zdt(local - offset as i128, offset, id)))
            }
            "with" => {
                let reject = self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                let names = [
                    "year", "month", "day", "hour", "minute", "second", "millisecond",
                    "microsecond", "nanosecond",
                ];
                let mut nf = f;
                let mut any = false;
                for (i, nm) in names.iter().enumerate() {
                    if let Some(x) = self.opt_int_field(a0, nm)? {
                        nf[i] = x;
                        any = true;
                    }
                }
                if !any {
                    return Err(Thrown("TypeError: with() requires a partial object".into()));
                }
                if !reject {
                    nf[1] = nf[1].clamp(1, 12);
                    nf[2] = nf[2].clamp(1, days_in_month(nf[0], nf[1]));
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
                // until: this → other; since: other → this.
                let (dt1, dt2) = if name == "until" { (f, o) } else { (o, f) };
                let df = difference_datetime(dt1, dt2, &largest);
                // With no calendar units (largestUnit ≤ day) the difference is an
                // exact nanosecond span: round it and re-balance. Calendar-unit
                // largestUnits keep the raw difference (full rounding deferred).
                if rank(&largest) >= rank("day") {
                    let total_ns = (df[3] as i128) * DAY_NS
                        + time_to_ns(&[df[4], df[5], df[6], df[7], df[8], df[9]]);
                    let inc_ns = unit_ns(&smallest) * inc;
                    let rounded = round_increment(total_ns, inc_ns, &mode);
                    Ok(Some(self.make_duration(balance_duration_ns(rounded, &largest))))
                } else if matches!(smallest.as_str(), "year" | "month" | "week") {
                    // Calendar-unit largest + smallest: round against the anchor
                    // calendar (time-of-day included via epoch nanoseconds).
                    let r = round_relative_datetime_diff(dt1, dt2, &smallest, &largest, &mode);
                    Ok(Some(self.make_duration(r)))
                } else {
                    Ok(Some(self.make_duration(df)))
                }
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
                Ok(Some(self.alloc_zdt(local - off as i128, off, id)))
            }
            "until" | "since" => {
                // Difference of two ZonedDateTimes (fixed-offset): the difference of
                // their local wall-clocks. Default largestUnit is "hour".
                let other = args.first().copied().unwrap_or(Value::UNDEFINED);
                let oz = self.zoned_date_time_from(other, Value::UNDEFINED)?;
                let of = self.zdt_local(oz.heap_index());
                let opts = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let all_units = &[
                    "auto", "year", "years", "month", "months", "week", "weeks", "day", "days",
                    "hour", "hours", "minute", "minutes", "second", "seconds", "millisecond",
                    "milliseconds", "microsecond", "microseconds", "nanosecond", "nanoseconds",
                ];
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
                    if rank(&smallest) < rank("hour") { smallest.clone() } else { "hour".to_string() }
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
                let f = self.zdt_local(idx);
                let (dt1, dt2) = if name == "until" { (f, of) } else { (of, f) };
                let df = difference_datetime(dt1, dt2, &largest);
                if rank(&largest) >= rank("day") {
                    let total_ns = (df[3] as i128) * DAY_NS
                        + time_to_ns(&[df[4], df[5], df[6], df[7], df[8], df[9]]);
                    let inc_ns = unit_ns(&smallest) * inc;
                    let rounded = round_increment(total_ns, inc_ns, &mode);
                    Ok(Some(self.make_duration(balance_duration_ns(rounded, &largest))))
                } else if matches!(smallest.as_str(), "year" | "month" | "week") {
                    // Calendar-unit largest + smallest: round against the anchor
                    // calendar (time-of-day included via epoch nanoseconds).
                    let r = round_relative_datetime_diff(dt1, dt2, &smallest, &largest, &mode);
                    Ok(Some(self.make_duration(r)))
                } else {
                    Ok(Some(self.make_duration(df)))
                }
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
                if !self.is_object_value(bag) {
                    return Err(Thrown("TypeError: ZonedDateTime.with requires an object".into()));
                }
                let mut f = self.zdt_local(idx);
                let names = [
                    "year", "month", "day", "hour", "minute", "second", "millisecond",
                    "microsecond", "nanosecond",
                ];
                for (i, nm) in names.iter().enumerate() {
                    if let Some(x) = self.opt_int_field(bag, nm)? {
                        f[i] = x;
                    }
                }
                // Validate the resolution options (disambiguation/offset/overflow).
                let options = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                self.read_zdt_options(options)?;
                let off = self.zdt_offset_ns(idx);
                let local = (iso_to_epoch_days(f[0], f[1], f[2]) as i128) * DAY_NS
                    + time_to_ns(&[f[3], f[4], f[5], f[6], f[7], f[8]]);
                let id = self.zdt_tz_id(idx).unwrap_or_else(|| "UTC".to_string());
                Ok(Some(self.alloc_zdt(local - off as i128, off, id)))
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
                let _ = self.read_overflow(options)?;
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
                self.validate_bag_offset_field(item)?;
                let reject = self.read_zdt_options(options)?;
                let f = self.to_plain_date_time_overflow(item, reject)?;
                let local = (iso_to_epoch_days(f[0], f[1], f[2]) as i128) * DAY_NS
                    + time_to_ns(&[f[3], f[4], f[5], f[6], f[7], f[8]]);
                return Ok(self.alloc_zdt(local - offset as i128, offset, id));
            }
        }
        let s = self.to_js_string(item)?;
        let _ = self.read_zdt_options(options)?;
        if !temporal_string_ok(&s, false, true) {
            return Err(Thrown(format!("RangeError: invalid ZonedDateTime string \"{s}\"")));
        }
        let (f, offset, id) = parse_zdt_string(&s)
            .ok_or_else(|| Thrown(format!("RangeError: invalid ZonedDateTime string \"{s}\"")))?;
        let local = (iso_to_epoch_days(f[0], f[1], f[2]) as i128) * DAY_NS
            + time_to_ns(&[f[3], f[4], f[5], f[6], f[7], f[8]]);
        Ok(self.alloc_zdt(local - offset as i128, offset, id))
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
    pub(crate) fn relative_to_dt(&mut self, rel: Value) -> Result<[i64; 9], Thrown> {
        if rel.is_heap() {
            if matches!(self.heap.get(rel.heap_index()), HeapObj::Temporal { kind: 7, .. }) {
                return Ok(self.zdt_local(rel.heap_index()));
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
                }
            }
        }
        self.to_plain_date_time(rel)
    }

    /// Validate a ZonedDateTime-like property bag's `offset` field: if present it
    /// must be a well-formed UTC-offset string (`±HH:MM…`). (The offset-vs-time-zone
    /// agreement check needs a tz database and is not done here.)
    pub(crate) fn validate_bag_offset_field(&mut self, bag: Value) -> Result<(), Thrown> {
        let offv = self.get_prop(bag, "offset")?;
        if offv != Value::UNDEFINED {
            // The offset must be a String or an object (which ToString-s); a
            // primitive non-string (null/boolean/number/bigint/symbol) is a TypeError.
            let is_string = offv.is_heap() && self.heap.is_str_like(offv.heap_index());
            if !is_string && !self.is_object_value(offv) {
                return Err(Thrown("TypeError: offset must be a string".into()));
            }
            let offs = self.to_js_string(offv)?;
            if !valid_offset_string(&offs) {
                return Err(Thrown(format!("RangeError: invalid offset string \"{offs}\"")));
            }
        }
        Ok(())
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
        if rel != Value::UNDEFINED {
            let start = self.relative_to_dt(rel)?;
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

    /// `Duration.round` with a relativeTo anchor: add the duration to the anchor,
    /// round the span to smallestUnit (calendar-aware for week/month/year via the
    /// anchor's variable unit lengths), then re-express from the anchor in
    /// largestUnit. Date-oriented; a sub-day remainder is rounded as nanoseconds.
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
        let sd = (start[0], start[1], start[2]);
        let sed = iso_to_epoch_days(sd.0, sd.1, sd.2);
        let eed = iso_to_epoch_days(end[0], end[1], end[2]);
        let sign: i64 = if eed >= sed { 1 } else { -1 };
        let total_ns = dt_epoch_ns(end) - dt_epoch_ns(start);

        // Express the span start→(date at epoch `e`) in largestUnit as [y,m,w,d].
        let express = |e: i64| -> [i64; 4] {
            let (ey, em, edd) = epoch_days_to_iso(e);
            difference_iso_date(sd, (ey, em, edd), largest)
        };
        // Round `whole` units of `unit_kind` (0=month,1=year) to the increment,
        // using the anchor's actual unit length for the fractional part.
        let mut round_calendar = |whole: i64, year: bool| -> i64 {
            let step = |n: i64| -> [i64; 10] {
                if year { [n, 0, 0, 0, 0, 0, 0, 0, 0, 0] } else { [0, n, 0, 0, 0, 0, 0, 0, 0, 0] }
            };
            let ml = dt_add_dur(start, step(whole));
            let ml1 = dt_add_dur(start, step(whole + sign));
            let mle = iso_to_epoch_days(ml[0], ml[1], ml[2]);
            let ml1e = iso_to_epoch_days(ml1[0], ml1[1], ml1[2]);
            let denom = (ml1e - mle).unsigned_abs().max(1) as i128;
            let num = (eed - mle) as i128;
            let scaled = whole as i128 * denom + num;
            (round_increment(scaled, inc * denom, mode) / denom) as i64
        };

        if ["year", "month", "week", "day"].contains(&smallest) {
            let rounded_end = match smallest {
                "day" => sed + round_increment((eed - sed) as i128, inc, mode) as i64,
                "week" => sed + (round_increment((eed - sed) as i128, 7 * inc, mode) / 7) as i64 * 7,
                "month" => {
                    let bal = difference_iso_date(sd, (end[0], end[1], end[2]), "month");
                    let rm = round_calendar(bal[0] * 12 + bal[1], false);
                    let re = dt_add_dur(start, [0, rm, 0, 0, 0, 0, 0, 0, 0, 0]);
                    iso_to_epoch_days(re[0], re[1], re[2])
                }
                _ => {
                    let bal = difference_iso_date(sd, (end[0], end[1], end[2]), "year");
                    let ry = round_calendar(bal[0], true);
                    let re = dt_add_dur(start, [ry, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                    iso_to_epoch_days(re[0], re[1], re[2])
                }
            };
            let b = express(rounded_end);
            Ok([b[0], b[1], b[2], b[3], 0, 0, 0, 0, 0, 0])
        } else {
            // Time smallestUnit (or the nanosecond default → pure balancing).
            let inc_ns = unit_ns(smallest) * inc;
            let rounded_ns = round_increment(total_ns, inc_ns, mode);
            let days = rounded_ns.div_euclid(DAY_NS);
            let t = ns_to_time(rounded_ns.rem_euclid(DAY_NS));
            let b = express(sed + days as i64);
            Ok([b[0], b[1], b[2], b[3], t[0], t[1], t[2], t[3], t[4], t[5]])
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
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}{}[{}]",
            f[0], f[1], f[2], f[3], f[4], f[5], frac, offset, tz
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
        instant_str_to_ns(s)
            .ok_or_else(|| Thrown(format!("RangeError: invalid instant string '{s}'")))
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
                let rounded = round_increment(ns, unit, &mode);
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
                let rounded = round_increment(ns, inc_ns, &mode);
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
        // Read both `month` and `monthCode` (alphabetical field order puts `month`
        // first). An invalid monthCode is a RangeError (not a silently-absent field);
        // when both are present they must agree.
        let month_opt = self.opt_int_field(obj, "month")?;
        let mc = self.get_prop(obj, "monthCode")?;
        if mc != Value::UNDEFINED {
            // monthCode is converted with ToPrimitive(string) then RequireString:
            // an object whose `toString`/`@@toPrimitive` yields a string is fine, but
            // a value that resolves to a non-string (number/bigint/boolean/symbol) is
            // a TypeError. A well-formed-but-invalid string is a RangeError below.
            let prim = self.to_primitive_string(mc)?;
            if !(prim.is_heap() && self.heap.is_str_like(prim.heap_index())) {
                return Err(Thrown("TypeError: monthCode must be a string".into()));
            }
            let s = self.heap.str_cow(prim.heap_index()).unwrap().into_owned();
            let code_month = parse_month_code(&s)
                .ok_or_else(|| Thrown(format!("RangeError: invalid monthCode '{s}'")))?;
            if let Some(m) = month_opt {
                if m != code_month {
                    return Err(Thrown("RangeError: month and monthCode must agree".into()));
                }
            }
            return Ok(Some(code_month));
        }
        Ok(month_opt)
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
        let (y, m, _ref) = match self.pym_fields(idx) {
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
                    format!("{}{}", iso_date_string(y, m, _ref), suf)
                };
                Ok(Some(self.alloc_str(s)))
            }
            "valueOf" => {
                Err(Thrown("TypeError: Called Temporal.PlainYearMonth.prototype.valueOf".into()))
            }
            "equals" => {
                let o = self.to_plain_year_month(a0)?;
                Ok(Some(Value::bool((y, m) == (o.0, o.1))))
            }
            "with" => {
                let reject = self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                let ny = self.opt_int_field(a0, "year")?.unwrap_or(y);
                let mut nm = self.read_month_field(a0)?.unwrap_or(m);
                if !reject {
                    nm = nm.clamp(1, 12);
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
                o.set("isoDay", Value::num(_ref as f64));
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
                let reject = self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
                let mut nm = self.read_month_field(a0)?.unwrap_or(m);
                let mut nd = self.opt_int_field(a0, "day")?.unwrap_or(d);
                if !reject {
                    nm = nm.clamp(1, 12);
                    nd = nd.clamp(1, days_in_month(ry, nm));
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
            tz_count += 1; // [Area/Location] time-zone annotation
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
    {
        return Some("iso8601".to_string());
    }
    None
}

/// Whether `s` is a well-formed UTC-offset string for a Temporal property-bag
/// `offset` field: a required sign, a 2-digit hour 00-23, then optional
/// minute / second components with a CONSISTENT separator style (all `:` or all
/// none), and an optional 1-9 digit sub-minute fraction. (Rejects "00:00" — no
/// sign, "+0" — short hour, "-000:00" — long hour, "+00:0000" — inconsistent.)
fn valid_offset_string(s: &str) -> bool {
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

/// Round the date-time difference dt1→dt2 to a calendar `smallest` unit
/// (year/month/week), then balance to `largest`. Like round_relative_date_diff
/// but the fraction toward the next unit is measured in epoch NANOSECONDS, so
/// the time-of-day contributes (NudgeToCalendarUnit for PlainDateTime/ZDT).
fn round_relative_datetime_diff(
    dt1: [i64; 9],
    dt2: [i64; 9],
    smallest: &str,
    largest: &str,
    mode: &str,
) -> [i64; 10] {
    let si = ["year", "month", "week"].iter().position(|&x| x == smallest).unwrap_or(2);
    let ns1 = dt_epoch_ns(dt1);
    let ns2 = dt_epoch_ns(dt2);
    let sign = (ns2 > ns1) as i64 - (ns2 < ns1) as i64;
    if sign == 0 {
        return [0; 10];
    }
    let count = difference_datetime(dt1, dt2, smallest)[si];
    let mk = |k: i64| -> [i64; 10] {
        let mut d = [0i64; 10];
        d[si] = k;
        d
    };
    let lower = dt_add_dur(dt1, mk(count));
    let ld = dt_epoch_ns(lower);
    let rounded = if ld == ns2 {
        count
    } else {
        let upper = dt_add_dur(dt1, mk(count + sign));
        let ud = dt_epoch_ns(upper);
        let progress = if ud != ld { (ns2 - ld) as f64 / (ud - ld) as f64 } else { 0.0 };
        round_fraction(count, sign, progress, mode)
    };
    let mut f = [0i64; 10];
    if si == 1 && largest == "year" {
        let end = dt_add_dur(dt1, mk(rounded));
        let d = difference_iso_date((dt1[0], dt1[1], dt1[2]), (end[0], end[1], end[2]), "year");
        f[..4].copy_from_slice(&d);
    } else {
        f[si] = rounded;
    }
    f
}

/// `Duration.total(unit)` relative to a start date-time: the (possibly fractional)
/// total of the duration measured in `unit`, computed via the calendar at `start`.
fn duration_total_relative(f: [i64; 10], start: [i64; 9], unit: &str) -> f64 {
    let end_ns = dt_epoch_ns(dt_add_dur(start, f));
    let start_ns = dt_epoch_ns(start);
    let diff = end_ns - start_ns;
    match unit {
        "year" | "month" => {
            let sign = if diff < 0 { -1i64 } else { 1 };
            let step: [i64; 10] = if unit == "year" {
                [sign, 0, 0, 0, 0, 0, 0, 0, 0, 0]
            } else {
                [0, sign, 0, 0, 0, 0, 0, 0, 0, 0]
            };
            let mut whole = 0i64;
            let mut cur = start;
            for _ in 0..2_000_000 {
                let next = dt_add_dur(cur, step);
                let nn = dt_epoch_ns(next);
                if (sign > 0 && nn > end_ns) || (sign < 0 && nn < end_ns) {
                    break;
                }
                cur = next;
                whole += sign;
            }
            let cur_ns = dt_epoch_ns(cur);
            let next_ns = dt_epoch_ns(dt_add_dur(cur, step));
            let frac = if next_ns != cur_ns {
                (end_ns - cur_ns) as f64 / (next_ns - cur_ns) as f64
            } else {
                0.0
            };
            whole as f64 + frac
        }
        "week" => diff as f64 / (7.0 * DAY_NS as f64),
        _ => diff as f64 / unit_ns(unit) as f64,
    }
}

/// Parse a ZonedDateTime ISO string `<date>[T<time>][±OFF|Z][tzid][annotations]`
/// into (date-time fields, offset ns, tz id). The `[tzid]` annotation is REQUIRED
/// (it carries the zone); a leading `!` critical flag is stripped. The explicit
/// numeric offset / `Z` is OPTIONAL — when absent the offset comes from the zone
/// (so `1970-01-01T00:00[UTC]` and `2020-01-01[+09:00]` parse). The time part is
/// optional (date-only -> midnight). Basic-format offsets (`-0800`) are accepted.
fn parse_zdt_string(s: &str) -> Option<([i64; 9], i64, String)> {
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
    let (time_str, offset_ns) = match time_part {
        None => ("", tz_offset),
        Some(t) => {
            if let Some(zpos) = t.find(['Z', 'z']) {
                (&t[..zpos], 0i64)
            } else if let Some(opos) = t.find(['+', '-']) {
                (&t[..opos], parse_offset_ns(&t[opos..])? as i64)
            } else {
                (t, tz_offset)
            }
        }
    };
    let time = if time_str.is_empty() { [0i64; 6] } else { parse_iso_time(time_str)? };
    let f = [date.0, date.1, date.2, time[0], time[1], time[2], time[3], time[4], time[5]];
    Some((f, offset_ns, tz_id))
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
