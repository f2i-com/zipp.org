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
        let mut f = [0i64; 10];
        for (i, slot) in f.iter_mut().enumerate() {
            let v = args.get(i).copied().unwrap_or(Value::UNDEFINED);
            if v != Value::UNDEFINED {
                let n = self.to_number(v)?;
                if !n.is_finite() || n.fract() != 0.0 {
                    return Err(Thrown(
                        "RangeError: Temporal.Duration fields must be integers".into(),
                    ));
                }
                *slot = n as i64;
            }
        }
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
                return parse_iso_duration(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid duration string '{s}'")));
            }
            if matches!(self.heap.get(idx), HeapObj::Object(_)) {
                let mut f = [0i64; 10];
                let mut any = false;
                for (i, name) in native::DURATION_FIELDS.iter().enumerate() {
                    let pv = self.get_prop(v, name)?;
                    if pv != Value::UNDEFINED {
                        any = true;
                        let n = self.to_number(pv)?;
                        if !n.is_finite() || n.fract() != 0.0 {
                            return Err(Thrown(
                                "RangeError: Temporal.Duration fields must be integers".into(),
                            ));
                        }
                        f[i] = n as i64;
                    }
                }
                if !any {
                    return Err(Thrown(
                        "TypeError: object is not a valid Temporal.Duration-like".into(),
                    ));
                }
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
                // Override the supplied fields (a plain partial-duration object).
                let mut nf = f;
                let mut any = false;
                for (i, name) in native::DURATION_FIELDS.iter().enumerate() {
                    let pv = self.get_prop(a0, name)?;
                    if pv != Value::UNDEFINED {
                        any = true;
                        let n = self.to_number(pv)?;
                        if !n.is_finite() || n.fract() != 0.0 {
                            return Err(Thrown("RangeError: Duration fields must be integers".into()));
                        }
                        nf[i] = n as i64;
                    }
                }
                if !any {
                    return Err(Thrown("TypeError: with() requires a partial Duration object".into()));
                }
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
                // No relativeTo support: years/months/weeks (in the value or as the
                // requested unit) need a calendar.
                if f[0] != 0 || f[1] != 0 || f[2] != 0 || matches!(unit.as_str(), "year" | "month" | "week") {
                    return Err(Thrown(
                        "RangeError: a relativeTo option is required for years, months, or weeks".into(),
                    ));
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
                let cal =
                    |u: &Option<String>| u.as_deref().is_some_and(|x| matches!(x, "year" | "month" | "week"));
                if f[0] != 0 || f[1] != 0 || f[2] != 0 || cal(&su) || cal(&lu) {
                    return Err(Thrown(
                        "RangeError: a relativeTo option is required for years, months, or weeks".into(),
                    ));
                }
                let inc = if options == Value::UNDEFINED {
                    1
                } else {
                    let v = self.get_prop(options, "roundingIncrement")?;
                    if v == Value::UNDEFINED {
                        1
                    } else {
                        let n = self.to_number(v)?;
                        if !n.is_finite() || n < 1.0 || n.fract() != 0.0 {
                            return Err(Thrown("RangeError: roundingIncrement out of range".into()));
                        }
                        n as i128
                    }
                };
                let mode = if options == Value::UNDEFINED {
                    "halfExpand".to_string()
                } else {
                    self.opt_string(
                        options,
                        "roundingMode",
                        "halfExpand",
                        &[
                            "ceil", "floor", "trunc", "expand", "halfCeil", "halfFloor", "halfTrunc",
                            "halfEven", "halfExpand",
                        ],
                    )?
                };
                let day_units =
                    ["day", "hour", "minute", "second", "millisecond", "microsecond", "nanosecond"];
                let rank = |u: &str| day_units.iter().position(|&x| x == u).unwrap_or(6) as i32;
                let smallest = su.unwrap_or_else(|| "nanosecond".to_string());
                if let Some(max) = max_increment(&smallest) {
                    if inc >= max || max % inc != 0 {
                        return Err(Thrown(
                            "RangeError: roundingIncrement must evenly divide the next unit".into(),
                        ));
                    }
                }
                let existing =
                    (3..10).filter(|&i| f[i] != 0).map(|i| (i - 3) as i32).min().unwrap_or(6);
                let largest =
                    lu.unwrap_or_else(|| day_units[existing.min(rank(&smallest)) as usize].to_string());
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
        if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) || !(-271821..=275760).contains(&y) {
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
        // A string value must be exactly "auto"; otherwise it is a Number 0..9.
        if fsd_v.is_heap() && self.heap.is_str_like(fsd_v.heap_index()) {
            if self.to_js_string(fsd_v)? == "auto" {
                return Ok((1, -1, false, mode));
            }
            return Err(Thrown("RangeError: fractionalSecondDigits must be 'auto' or 0..9".into()));
        }
        let n = self.to_number(fsd_v)?;
        if n.is_nan() {
            return Err(Thrown("RangeError: fractionalSecondDigits is NaN".into()));
        }
        let n = n.trunc() as i64;
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
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                return parse_iso_date(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid date string '{s}'")));
            }
            if matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
                let yv = self.get_prop(v, "year")?;
                let mv = self.get_prop(v, "month")?;
                let dv = self.get_prop(v, "day")?;
                if yv == Value::UNDEFINED || mv == Value::UNDEFINED || dv == Value::UNDEFINED {
                    return Err(Thrown("TypeError: PlainDate-like requires year, month, day".into()));
                }
                let (y, mut m, mut d) =
                    (self.to_number(yv)? as i64, self.to_number(mv)? as i64, self.to_number(dv)? as i64);
                if reject {
                    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
                        return Err(Thrown("RangeError: invalid date fields".into()));
                    }
                } else {
                    m = m.clamp(1, 12);
                    d = d.clamp(1, days_in_month(y, m));
                }
                return Ok((y, m, d));
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.PlainDate".into()))
    }

    /// `date ± duration` (date units constrain day; time units fold to whole days).
    pub(crate) fn date_add(&self, y: i64, m: i64, d: i64, dur: &[i64; 10], sign: i64) -> (i64, i64, i64) {
        let total_months = (y + dur[0] * sign) * 12 + (m - 1) + dur[1] * sign;
        let ny = total_months.div_euclid(12);
        let nm = total_months.rem_euclid(12) + 1;
        let nd = d.min(days_in_month(ny, nm));
        let time_ns = (dur[4] as i128) * 3_600_000_000_000
            + (dur[5] as i128) * 60_000_000_000
            + (dur[6] as i128) * 1_000_000_000
            + (dur[7] as i128) * 1_000_000
            + (dur[8] as i128) * 1_000
            + (dur[9] as i128);
        let extra_days = (time_ns / 86_400_000_000_000) as i64;
        let ed = iso_to_epoch_days(ny, nm, nd) + (dur[2] * 7 + dur[3] + extra_days) * sign;
        epoch_days_to_iso(ed)
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
                let sign = if name == "add" { 1 } else { -1 };
                let (ny, nm, nd) = self.date_add(y, m, d, &dur, sign);
                Ok(Some(self.make_plain_date(ny, nm, nd)?))
            }
            "until" | "since" => {
                let other = self.to_plain_date(a0)?;
                let a1 = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let largest = self.opt_string(
                    a1,
                    "largestUnit",
                    "auto",
                    &[
                        "auto", "year", "years", "month", "months", "week", "weeks", "day", "days",
                    ],
                )?;
                let largest = normalize_unit(&largest, "day");
                // until: this → other; since: other → this.
                let (d1, d2) = if name == "until" {
                    ((y, m, d), other)
                } else {
                    (other, (y, m, d))
                };
                let diff = difference_iso_date(d1, d2, &largest);
                let mut f = [0i64; 10];
                f[0] = diff[0];
                f[1] = diff[1];
                f[2] = diff[2];
                f[3] = diff[3];
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

    /// Read an optional integer field from an options/with object (None if absent).
    pub(crate) fn opt_int_field(&mut self, obj: Value, key: &str) -> Result<Option<i64>, Thrown> {
        let v = self.get_prop(obj, key)?;
        if v == Value::UNDEFINED {
            Ok(None)
        } else {
            Ok(Some(self.to_number(v)? as i64))
        }
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
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                return parse_iso_time(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid time string '{s}'")));
            }
            if matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
                let names =
                    ["hour", "minute", "second", "millisecond", "microsecond", "nanosecond"];
                let maxes = [23, 59, 59, 999, 999, 999];
                let mut f = [0i64; 6];
                for (i, nm) in names.iter().enumerate() {
                    if let Some(x) = self.opt_int_field(v, nm)? {
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
                return parse_iso_datetime(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid datetime string '{s}'")));
            }
            if matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
                let names = [
                    "year", "month", "day", "hour", "minute", "second", "millisecond",
                    "microsecond", "nanosecond",
                ];
                let mut f = [0i64; 9];
                let mut have_date = [false; 3];
                for (i, nm) in names.iter().enumerate() {
                    if let Some(x) = self.opt_int_field(v, nm)? {
                        f[i] = x;
                        if i < 3 {
                            have_date[i] = true;
                        }
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
                    f[1] = f[1].clamp(1, 12);
                    f[2] = f[2].clamp(1, days_in_month(f[0], f[1]));
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
                let o = self.to_plain_date_time(a0)?;
                Ok(Some(Value::bool(f == o)))
            }
            "toPlainDate" => Ok(Some(self.make_plain_date(date[0], date[1], date[2])?)),
            "toPlainTime" => Ok(Some(self.make_plain_time(time)?)),
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
                // Date part: years/months constrain, then weeks/days + carry.
                let tm = (date[0] + dur[0] * sign) * 12 + (date[1] - 1) + dur[1] * sign;
                let ny0 = tm.div_euclid(12);
                let nmo = tm.rem_euclid(12) + 1;
                let nd0 = date[2].min(days_in_month(ny0, nmo));
                let ed = iso_to_epoch_days(ny0, nmo, nd0) + (dur[2] * 7 + dur[3]) * sign + carry;
                let (ny, nm, nd) = epoch_days_to_iso(ed);
                Ok(Some(self.make_plain_date_time([
                    ny, nm, nd, nt[0], nt[1], nt[2], nt[3], nt[4], nt[5],
                ])?))
            }
            "until" | "since" => {
                let o = self.to_plain_date_time(a0)?;
                let a1 = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                let largest = self.opt_string(
                    a1,
                    "largestUnit",
                    "auto",
                    &[
                        "auto", "year", "years", "month", "months", "week", "weeks", "day", "days",
                        "hour", "hours", "minute", "minutes", "second", "seconds", "millisecond",
                        "milliseconds", "microsecond", "microseconds", "nanosecond", "nanoseconds",
                    ],
                )?;
                let largest = normalize_unit(&largest, "day");
                // until: this → other; since: other → this.
                let (dt1, dt2) = if name == "until" { (f, o) } else { (o, f) };
                let df = difference_datetime(dt1, dt2, &largest);
                Ok(Some(self.make_duration(df)))
            }
            "round" => {
                let (su, inc, mode) = self.read_round_options(
                    a0,
                    &[
                        "day", "hour", "minute", "second", "millisecond", "microsecond",
                        "nanosecond",
                    ],
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

    pub(crate) fn to_instant_ns(&mut self, v: Value) -> Result<i128, Thrown> {
        if v.is_heap() {
            if let Some(ns) = self.instant_ns(v.heap_index()) {
                return Ok(ns);
            }
            if self.heap.is_str_like(v.heap_index()) {
                let s = self.heap.str_cow(v.heap_index()).unwrap().into_owned();
                return instant_str_to_ns(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid instant string '{s}'")));
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.Instant".into()))
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
                // Default time zone is UTC ("Z"); a timeZone option is not yet supported.
                let (unit, digits, omit, mode) = self.time_precision(a0)?;
                let rounded = round_increment(ns, unit, &mode);
                let t = ns_to_time(rounded.rem_euclid(DAY_NS));
                let (y, mo, d) = epoch_days_to_iso(rounded.div_euclid(DAY_NS) as i64);
                let s = format!("{}T{}Z", iso_date_string(y, mo, d), format_time_part(&t, digits, omit));
                Ok(Some(self.alloc_str(s)))
            }
            "valueOf" => Err(Thrown("TypeError: Called Temporal.Instant.prototype.valueOf".into())),
            "equals" => {
                let o = self.to_instant_ns(a0)?;
                Ok(Some(Value::bool(ns == o)))
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
        if !(1..=12).contains(&m) || !(-271821..=275760).contains(&y) {
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
                return parse_iso_year_month(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid year-month string '{s}'")));
            }
            if matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
                let yv = self.get_prop(v, "year")?;
                let m = self.read_month_field(v)?;
                if yv == Value::UNDEFINED || m.is_none() {
                    return Err(Thrown(
                        "TypeError: PlainYearMonth-like requires year and month".into(),
                    ));
                }
                let y = self.to_number(yv)? as i64;
                let mut m = m.unwrap();
                if reject {
                    if !(1..=12).contains(&m) {
                        return Err(Thrown("RangeError: month out of range".into()));
                    }
                } else {
                    m = m.clamp(1, 12);
                }
                return Ok((y, m, 1));
            }
        }
        Err(Thrown("TypeError: cannot convert value to a Temporal.PlainYearMonth".into()))
    }

    /// Read month from an object: monthCode ("M06") takes precedence over `month`.
    pub(crate) fn read_month_field(&mut self, obj: Value) -> Result<Option<i64>, Thrown> {
        let mc = self.get_prop(obj, "monthCode")?;
        if mc != Value::UNDEFINED {
            let s = self.to_js_string(mc)?;
            return Ok(parse_month_code(&s));
        }
        self.opt_int_field(obj, "month")
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
                let sign = if name == "add" { 1 } else { -1 };
                let op_sign = sign * Self::duration_sign(&dur);
                // Reference day per spec: start of month for non-negative ops, end of
                // month for negative — so day/week units don't spill into a wrong month.
                let ref_day = if op_sign < 0 { days_in_month(y, m) } else { 1 };
                let (ny, nm, _nd) = self.date_add(y, m, ref_day, &dur, sign);
                Ok(Some(self.make_plain_year_month(ny, nm, 1)?))
            }
            "until" | "since" => {
                let o = self.to_plain_year_month(a0)?;
                let from = y * 12 + (m - 1);
                let to = o.0 * 12 + (o.1 - 1);
                let diff = if name == "until" { to - from } else { from - to };
                let mut f = [0i64; 10];
                f[0] = diff / 12;
                f[1] = diff % 12;
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
                return parse_iso_month_day(&s)
                    .ok_or_else(|| Thrown(format!("RangeError: invalid month-day string '{s}'")));
            }
            if matches!(self.heap.get(v.heap_index()), HeapObj::Object(_)) {
                let m = self.read_month_field(v)?;
                let dv = self.get_prop(v, "day")?;
                if m.is_none() || dv == Value::UNDEFINED {
                    return Err(Thrown(
                        "TypeError: PlainMonthDay-like requires month and day".into(),
                    ));
                }
                let mut m = m.unwrap();
                let mut d = self.to_number(dv)? as i64;
                if reject {
                    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(1972, m) {
                        return Err(Thrown("RangeError: month-day out of range".into()));
                    }
                } else {
                    m = m.clamp(1, 12);
                    d = d.clamp(1, days_in_month(1972, m));
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
