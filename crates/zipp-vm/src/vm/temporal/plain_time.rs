// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
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
        let idx = self.heap.alloc(HeapObj::Temporal {
            kind: 2,
            fields: f.to_vec(),
        });
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
        self.to_plain_time_overflow(v, None)
    }

    pub(crate) fn to_plain_time_overflow(
        &mut self,
        v: Value,
        options: Option<Value>,
    ) -> Result<[i64; 6], Thrown> {
        if v.is_heap() {
            if let Some(f) = self.plain_time_fields(v.heap_index()) {
                if let Some(o) = options {
                    self.read_overflow(o)?;
                }
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
                    if let Some(o) = options {
                        self.read_overflow(o)?;
                    }
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
                // PrepareTemporalFields reads fields in ALPHABETICAL order (observable
                // via getter side effects): hour, microsecond, millisecond, minute,
                // nanosecond, second — each into its canonical slot (0=hour 1=minute
                // 2=second 3=ms 4=us 5=ns) with its max.
                let fields: [(&str, usize, i64); 6] = [
                    ("hour", 0, 23),
                    ("microsecond", 4, 999),
                    ("millisecond", 3, 999),
                    ("minute", 1, 59),
                    ("nanosecond", 5, 999),
                    ("second", 2, 59),
                ];
                // Phase 1: read all field GETs (observable, alphabetical order).
                let mut raw: [Option<i64>; 6] = [None; 6];
                let mut any = false;
                for &(nm, slot, _) in &fields {
                    if let Some(x) = self.opt_int_field(v, nm)? {
                        raw[slot] = Some(x);
                        any = true;
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
                // GetTemporalOverflowOption AFTER the field GETs, before the range
                // validation; absent options → constrain.
                let reject = if let Some(o) = options {
                    self.read_overflow(o)?
                } else {
                    false
                };
                // Phase 2: apply reject/constrain (pure — no observable side effects).
                let mut f = [0i64; 6];
                for &(nm, slot, mx) in &fields {
                    if let Some(x) = raw[slot] {
                        if reject {
                            if x < 0 || x > mx {
                                return Err(Thrown(format!("RangeError: {nm} out of range")));
                            }
                            f[slot] = x;
                        } else {
                            f[slot] = x.clamp(0, mx);
                        }
                    }
                }
                return Ok(f);
            }
        }
        Err(Thrown(
            "TypeError: cannot convert value to a Temporal.PlainTime".into(),
        ))
    }

    pub(crate) fn plain_time_method(
        &mut self,
        idx: u32,
        name: &str,
        args: &[Value],
    ) -> Result<Option<Value>, Thrown> {
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
            "valueOf" => Err(Thrown(
                "TypeError: Called Temporal.PlainTime.prototype.valueOf".into(),
            )),
            "equals" => {
                let o = self.to_plain_time(a0)?;
                Ok(Some(Value::bool(f == o)))
            }
            "with" => {
                self.reject_temporal_like(a0)?;
                // PrepareTemporalFields reads ALPHABETICALLY (observable getters,
                // slot-mapped to [h,mi,s,ms,us,ns]) BEFORE the options bag.
                let fields: [(&str, usize); 6] = [
                    ("hour", 0),
                    ("microsecond", 4),
                    ("millisecond", 3),
                    ("minute", 1),
                    ("nanosecond", 5),
                    ("second", 2),
                ];
                let maxes = [23, 59, 59, 999, 999, 999];
                let mut raw: [Option<i64>; 6] = [None; 6];
                let mut any = false;
                for &(nm, slot) in &fields {
                    if let Some(x) = self.opt_int_field(a0, nm)? {
                        raw[slot] = Some(x);
                        any = true;
                    }
                }
                if !any {
                    return Err(Thrown(
                        "TypeError: with() requires a partial time object".into(),
                    ));
                }
                let reject =
                    self.read_overflow(args.get(1).copied().unwrap_or(Value::UNDEFINED))?;
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
                Ok(Some(
                    self.make_duration(balance_duration_ns(rounded, &largest)?),
                ))
            }
            "round" => {
                let (su, inc, mode) = self.read_round_options(
                    a0,
                    &[
                        "hour",
                        "minute",
                        "second",
                        "millisecond",
                        "microsecond",
                        "nanosecond",
                    ],
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
                Ok(Some(Value::heap(
                    self.heap.alloc(HeapObj::Object(Box::new(o))),
                )))
            }
            _ => Ok(None),
        }
    }

    // ── Temporal.PlainDateTime ──
}
