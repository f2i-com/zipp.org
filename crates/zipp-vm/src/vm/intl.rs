#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// Read an internal slot stored on an Intl instance's `resolved` object.
    pub(crate) fn intl_slot(&self, resolved: u32, key: &str) -> Value {
        if let HeapObj::Object(m) = self.heap.get(resolved) {
            if let Some(i) = m.pos(key) {
                return m.vals[i];
            }
        }
        Value::UNDEFINED
    }

    /// An object's own property (attr + value) if present — used to resolve and
    /// invoke prototype accessor getters for Intl instances.
    pub(crate) fn own_member(&self, idx: u32, key: &str) -> Option<(PropAttr, Value)> {
        if let HeapObj::Object(m) = self.heap.get(idx) {
            if let Some(i) = m.pos(key) {
                return Some((m.attrs[i], m.vals[i]));
            }
        }
        None
    }

    /// Brand-check `this` as an Intl instance of `kind`; return its `resolved` idx.
    pub(crate) fn intl_this(&self, this: Value, kind: u8, m: &str) -> Result<u32, Thrown> {
        if this.is_heap() {
            if let HeapObj::Intl { kind: k, resolved } = self.heap.get(this.heap_index()) {
                if *k == kind {
                    return Ok(*resolved);
                }
            }
        }
        Err(Thrown(format!("TypeError: {m} called on an incompatible receiver")))
    }

    /// Shallow-copy an object's own enumerable data properties into a fresh object
    /// (preserving insertion order) — used by resolvedOptions().
    pub(crate) fn clone_plain_object(&mut self, src: u32) -> Value {
        let pairs: Vec<(String, Value)> = match self.heap.get(src) {
            HeapObj::Object(m) => (0..m.keys.len())
                .filter(|&i| !m.attrs[i].accessor && !is_hidden_key(&m.keys[i]))
                .map(|i| (m.keys[i].clone(), m.vals[i]))
                .collect(),
            _ => vec![],
        };
        let mut o = ObjMap::new();
        for (k, v) in pairs {
            // resolvedOptions() must hand back a FRESH object graph each call:
            // `pluralCategories` is an array, and two calls returning the same
            // array would let a caller mutate the instance's slot.
            let v = match v.is_heap().then(|| self.heap.get(v.heap_index())) {
                Some(HeapObj::Array(items)) => {
                    let items = items.clone();
                    Value::heap(self.heap.alloc(HeapObj::Array(items)))
                }
                _ => v,
            };
            o.set(&k, v);
        }
        Value::heap(self.heap.alloc(HeapObj::Object(Box::new(o))))
    }

    /// The `[[Locale]]` slot of an Intl.Locale instance, if `v` is one.
    /// CanonicalizeLocaleList and the Locale constructor both take the FULL
    /// canonical tag from a Locale argument, extensions included — not its
    /// baseName.
    pub(crate) fn locale_slot_tag(&self, v: Value) -> Option<String> {
        if !v.is_heap() {
            return None;
        }
        if let HeapObj::Intl { kind: native::INTL_LOCALE, resolved } = *self.heap.get(v.heap_index()) {
            return Some(self.display(self.intl_slot(resolved, "@@tag")));
        }
        None
    }

    /// CanonicalizeLocaleList(locales) (ECMA-402 9.2.1). `undefined` → empty; a
    /// String or Intl.Locale → a one-element list; anything else is ToObject'd
    /// and walked as an array-like.
    ///
    /// The element type check is load-bearing: only a String or an Object may
    /// appear (`[undefined]`, `[null]`, `[1]`, `[Symbol()]` are each a
    /// TypeError), and a hole is skipped via HasProperty rather than being
    /// coerced. Reading a non-heap element's `heap_index()` used to index the
    /// heap directly, which panicked outright on `[0.1]`.
    pub(crate) fn canonicalize_locale_list(&mut self, locales: Value) -> Result<Vec<String>, Thrown> {
        let mut out: Vec<String> = vec![];
        if locales == Value::UNDEFINED {
            return Ok(out);
        }
        if let Some(tag) = self.locale_slot_tag(locales) {
            out.push(tag);
            return Ok(out);
        }
        if locales.is_heap() && self.heap.is_str_like(locales.heap_index()) {
            let s = self.heap.str_cow(locales.heap_index()).unwrap().into_owned();
            out.push(canonicalize_locale(&s).ok_or_else(|| {
                Thrown(format!("RangeError: Incorrect locale information provided: {s}"))
            })?);
            return Ok(out);
        }
        // Step 4: ToObject — `null`/`undefined` are a TypeError here, not an
        // empty list, and a primitive boxes into a wrapper with no indices.
        self.require_object_coercible(locales)?;
        let obj = self.to_object(locales)?;
        let len_v = self.get_prop(obj, "length")?;
        // ToLength: NaN/negative clamp to 0, and the loop caps at a length no
        // array-like can actually reach so a bogus 2^53 does not hang.
        // ToPrimitive FIRST for an OBJECT length — the infallible `to_number`
        // cannot run a user `valueOf`/`toString`, so `{length: {valueOf(){throw}}}`
        // silently read as an empty list instead of propagating. (Primitives skip
        // it: `to_primitive_number(undefined)` would try to read its `valueOf`.)
        let len_v = if self.is_object_value(len_v) {
            self.to_primitive_number(len_v)?
        } else {
            len_v
        };
        let len_f = self.to_number(len_v)?;
        let len = if len_f.is_nan() || len_f <= 0.0 { 0u64 } else { len_f.min(9.007e15) as u64 };
        for i in 0..len {
            let key = Value::num(i as f64);
            if !self.has_property_dyn(obj, key)? {
                continue;
            }
            let el = self.get_index(obj, key)?;
            if let Some(tag) = self.locale_slot_tag(el) {
                if !out.contains(&tag) {
                    out.push(tag);
                }
                continue;
            }
            let is_string = el.is_heap() && self.heap.is_str_like(el.heap_index());
            if !is_string && !self.is_object_value(el) {
                return Err(Thrown(
                    "TypeError: locale list elements must be strings or objects".into(),
                ));
            }
            let s = self.to_js_string(el)?;
            let c = canonicalize_locale(&s).ok_or_else(|| {
                Thrown(format!("RangeError: Incorrect locale information provided: {s}"))
            })?;
            if !out.contains(&c) {
                out.push(c);
            }
        }
        Ok(out)
    }

    /// Pick the resolved locale: the first requested tag (we "support" all), else
    /// the default "en".
    pub(crate) fn resolve_locale(&mut self, locales: Value) -> Result<String, Thrown> {
        let list = self.canonicalize_locale_list(locales)?;
        Ok(list.into_iter().next().unwrap_or_else(|| "en".to_string()))
    }

    /// Read + ToString-cast a string option (returns `default` if undefined),
    /// WITHOUT any allowed-list validation. The Temporal GetDifferenceSettings
    /// order-of-operations reads (and casts) every option before validating any,
    /// so callers that must defer the disallowed-value RangeError read with this
    /// and validate afterward (`unit_allowed`).
    pub(crate) fn opt_string_raw(
        &mut self,
        options: Value,
        key: &str,
        default: &str,
    ) -> Result<String, Thrown> {
        if options == Value::UNDEFINED {
            return Ok(default.to_string());
        }
        let v = self.get_prop(options, key)?;
        if v == Value::UNDEFINED {
            return Ok(default.to_string());
        }
        self.to_js_string(v)
    }

    /// RangeError if `s` is not in `allowed` (when non-empty) — the deferred
    /// validation half of `opt_string_raw`.
    pub(crate) fn unit_allowed(&self, s: &str, key: &str, allowed: &[&str]) -> Result<(), Thrown> {
        if !allowed.is_empty() && !allowed.contains(&s) {
            return Err(Thrown(format!("RangeError: Value {s} out of range for option {key}")));
        }
        Ok(())
    }

    /// Read a string option (returns `default` if undefined); validates against
    /// `allowed` when non-empty (→ RangeError) — read and validate in one step.
    ///
    /// GetOption (ECMA-402 9.2.13) step 3 returns `default` for an ABSENT option
    /// *without* consulting `values`: only a value the caller actually supplied is
    /// range-checked. Validating the default too made every componentless
    /// `Intl.DateTimeFormat(loc, {})` throw on its own `""` weekday.
    pub(crate) fn opt_string(
        &mut self,
        options: Value,
        key: &str,
        default: &str,
        allowed: &[&str],
    ) -> Result<String, Thrown> {
        if options == Value::UNDEFINED {
            return Ok(default.to_string());
        }
        let v = self.get_prop(options, key)?;
        if v == Value::UNDEFINED {
            return Ok(default.to_string());
        }
        let s = self.to_js_string(v)?;
        self.unit_allowed(&s, key, allowed)?;
        Ok(s)
    }

    /// GetOption(options, key, string, values, undefined) — `None` when the option
    /// is absent. Distinct from `opt_string`, which folds "absent" into a default
    /// string: ECMA-402 needs the two apart (an absent `currency` is a TypeError
    /// under style "currency", an explicit "" is a RangeError).
    pub(crate) fn opt_string_opt(
        &mut self,
        options: Value,
        key: &str,
        allowed: &[&str],
    ) -> Result<Option<String>, Thrown> {
        if options == Value::UNDEFINED {
            return Ok(None);
        }
        let v = self.get_prop(options, key)?;
        if v == Value::UNDEFINED {
            return Ok(None);
        }
        let s = self.to_js_string(v)?;
        self.unit_allowed(&s, key, allowed)?;
        Ok(Some(s))
    }

    /// GetOption(options, key, boolean, empty, undefined) — ToBoolean of a present
    /// value, `None` when absent (DateTimeFormat's `hour12` distinguishes the two:
    /// only a PRESENT hour12 overrides hourCycle).
    pub(crate) fn opt_bool_opt(&mut self, options: Value, key: &str) -> Result<Option<bool>, Thrown> {
        if options == Value::UNDEFINED {
            return Ok(None);
        }
        let v = self.get_prop(options, key)?;
        if v == Value::UNDEFINED {
            return Ok(None);
        }
        Ok(Some(self.truthy(v)))
    }

    /// GetNumberOption(options, key, min, max, undefined) — `None` when absent.
    /// DefaultNumberOption floors the coerced value, so 1.9 → 1.
    pub(crate) fn opt_int_opt(
        &mut self,
        options: Value,
        key: &str,
        min: i64,
        max: i64,
    ) -> Result<Option<i64>, Thrown> {
        if options == Value::UNDEFINED {
            return Ok(None);
        }
        let v = self.get_prop(options, key)?;
        self.default_number_option(v, key, min, max)
    }

    /// DefaultNumberOption(value, min, max, undefined): ToNumber then a NaN/range
    /// check, floored. Split out because SetNumberFormatDigitOptions reads the four
    /// digit options RAW (in one block) and only range-checks them later.
    pub(crate) fn default_number_option(
        &mut self,
        v: Value,
        key: &str,
        min: i64,
        max: i64,
    ) -> Result<Option<i64>, Thrown> {
        if v == Value::UNDEFINED {
            return Ok(None);
        }
        let n = self.to_number(v)?;
        if n.is_nan() || n < min as f64 || n > max as f64 {
            return Err(Thrown(format!("RangeError: {key} value is out of range")));
        }
        Ok(Some(n.floor() as i64))
    }

    /// Read an integer option clamped to [min,max] (returns `default` if undefined).
    pub(crate) fn opt_int(
        &mut self,
        options: Value,
        key: &str,
        default: i64,
        min: i64,
        max: i64,
    ) -> Result<i64, Thrown> {
        if options == Value::UNDEFINED {
            return Ok(default);
        }
        let v = self.get_prop(options, key)?;
        if v == Value::UNDEFINED {
            return Ok(default);
        }
        let n = self.to_number(v)?;
        if n.is_nan() || n < min as f64 || n > max as f64 {
            return Err(Thrown(format!("RangeError: {key} value is out of range")));
        }
        Ok(n as i64)
    }

    /// Read a Temporal round() options bag (or a bare smallestUnit string):
    /// returns (smallestUnit, roundingIncrement, roundingMode), validated.
    pub(crate) fn read_round_options(
        &mut self,
        arg: Value,
        allowed: &[&str],
        validate_increment: bool,
    ) -> Result<(String, i128, String), Thrown> {
        // A bare string argument is shorthand for { smallestUnit: <string> }; any
        // other non-object (a number/boolean/etc.) is a TypeError, not options.
        let (su_string, options) =
            if arg.is_heap() && self.heap.is_str_like(arg.heap_index()) {
                (Some(arg), Value::UNDEFINED)
            } else if arg == Value::UNDEFINED {
                return Err(Thrown("TypeError: round() requires an options argument".into()));
            } else if !self.is_object_value(arg) {
                return Err(Thrown(
                    "TypeError: round() options must be an object or a string".into(),
                ));
            } else {
                (None, arg)
            };
        // Spec order: read + coerce ALL options BEFORE any algorithmic validation —
        // roundingIncrement, then roundingMode, then smallestUnit (the observable
        // get/valueOf/toString sequence the order-of-operations tests assert).
        let inc = self.read_rounding_increment(options)?;
        let mode = if options == Value::UNDEFINED {
            "halfExpand".to_string()
        } else {
            self.read_rounding_mode(options, "halfExpand")?
        };
        let smallest_v = match su_string {
            Some(s) => s,
            None => self.get_prop(options, "smallestUnit")?,
        };
        if smallest_v == Value::UNDEFINED {
            return Err(Thrown("RangeError: smallestUnit is required".into()));
        }
        let su = normalize_unit(&self.to_js_string(smallest_v)?, "");
        if !allowed.contains(&su.as_str()) {
            return Err(Thrown(format!("RangeError: invalid smallestUnit: {su}")));
        }
        // Algorithmic validation comes last: the increment must evenly divide its
        // unit. Instant.round validates against the solar day instead (its own
        // guard downstream), so it opts out with validate_increment = false.
        if validate_increment {
            if su == "day" {
                // "day" rounds against a 1-day dividend (inclusive), so only an
                // increment of exactly 1 is valid.
                if inc != 1 {
                    return Err(Thrown(
                        "RangeError: roundingIncrement must evenly divide the next unit".into(),
                    ));
                }
            } else if let Some(max) = max_increment(&su) {
                if inc >= max || max % inc != 0 {
                    return Err(Thrown(
                        "RangeError: roundingIncrement must evenly divide the next unit".into(),
                    ));
                }
            }
        }
        Ok((su, inc, mode))
    }

    /// Read until()/since() options for time-only types (PlainTime/Instant):
    /// largestUnit (default `default_largest`), smallestUnit (default nanosecond),
    /// roundingIncrement, roundingMode (default trunc). Returns the resolved
    /// (largestUnit, smallestUnit, increment, mode); errors if largest < smallest.
    pub(crate) fn read_time_diff_options(
        &mut self,
        options: Value,
        default_largest: &str,
    ) -> Result<(String, String, i128, String), Thrown> {
        let units = ["hour", "minute", "second", "millisecond", "microsecond", "nanosecond"];
        let rank = |u: &str| units.iter().position(|&x| x == u).unwrap_or(5) as i32;
        if options == Value::UNDEFINED {
            return Ok((default_largest.to_string(), "nanosecond".to_string(), 1, "trunc".to_string()));
        }
        if !self.is_object_value(options) {
            return Err(Thrown("TypeError: options must be an object or undefined".into()));
        }
        let largest_allowed = [
            "auto", "hour", "hours", "minute", "minutes", "second", "seconds", "millisecond",
            "milliseconds", "microsecond", "microseconds", "nanosecond", "nanoseconds",
        ];
        let small_allowed = &largest_allowed[1..]; // same minus "auto"
        // GetDifferenceSettings reads the options in this exact order: largestUnit,
        // roundingIncrement, roundingMode, then smallestUnit — and only AFTER all four
        // are read does it resolve an "auto" largestUnit and run the range validations.
        // Read+cast all four with no validation, THEN validate the unit lists — so a
        // disallowed largestUnit does not throw before smallestUnit is even read.
        let lu_raw = self.opt_string_raw(options, "largestUnit", "auto")?;
        let inc = self.read_rounding_increment(options)?;
        let mode = self.read_rounding_mode(options, "trunc")?;
        let su_raw = self.opt_string_raw(options, "smallestUnit", "nanosecond")?;
        self.unit_allowed(&lu_raw, "largestUnit", &largest_allowed)?;
        self.unit_allowed(&su_raw, "smallestUnit", small_allowed)?;
        let su = normalize_unit(&su_raw, "nanosecond");
        // An "auto"/absent largestUnit resolves to LargerOfTwoTemporalUnits(default,
        // smallestUnit) — the lower rank — so e.g. smallestUnit "hour" yields
        // largestUnit "hour" instead of wrongly defaulting to the (smaller) default.
        let lu = if normalize_unit(&lu_raw, "auto") == "auto" {
            if rank(default_largest) <= rank(&su) { default_largest.to_string() } else { su.clone() }
        } else {
            normalize_unit(&lu_raw, default_largest)
        };
        if rank(&lu) > rank(&su) {
            return Err(Thrown(
                "RangeError: largestUnit must not be smaller than smallestUnit".into(),
            ));
        }
        if let Some(max) = max_increment(&su) {
            if inc >= max || max % inc != 0 {
                return Err(Thrown(
                    "RangeError: roundingIncrement must evenly divide the next unit".into(),
                ));
            }
        }
        Ok((lu, su, inc, mode))
    }

    /// SetNumberFormatDigitOptions (ECMA-402 15.1.3) — shared verbatim by
    /// Intl.NumberFormat and Intl.PluralRules. Reads nine options in one fixed
    /// order (the `constructor-option-read-order` tests observe every Get), then
    /// resolves which of the fraction-digit / significant-digit pairs actually
    /// apply. Returns the resolved slots; the caller decides where they land in
    /// its own resolvedOptions table.
    fn read_number_format_digit_options(
        &mut self,
        options: Value,
        mnfd_default: i64,
        mxfd_default: i64,
        notation: &str,
    ) -> Result<DigitOptions, Thrown> {
        let mnid = self.opt_int(options, "minimumIntegerDigits", 1, 1, 21)?;
        // Steps 2-5 read the four digit options RAW: the range checks happen later
        // (and only for the pair that is actually needed), so a bogus
        // maximumSignificantDigits is silently ignored when roundingPriority left
        // significant digits out of play.
        let get = |vm: &mut Self, k: &str| -> Result<Value, Thrown> {
            if options == Value::UNDEFINED { Ok(Value::UNDEFINED) } else { vm.get_prop(options, k) }
        };
        let mnfd_raw = get(self, "minimumFractionDigits")?;
        let mxfd_raw = get(self, "maximumFractionDigits")?;
        let mnsd_raw = get(self, "minimumSignificantDigits")?;
        let mxsd_raw = get(self, "maximumSignificantDigits")?;
        let rounding_increment = self.opt_int(options, "roundingIncrement", 1, 1, 5000)?;
        if !matches!(
            rounding_increment,
            1 | 2 | 5 | 10 | 20 | 25 | 50 | 100 | 200 | 250 | 500 | 1000 | 2000 | 2500 | 5000
        ) {
            return Err(Thrown("RangeError: invalid roundingIncrement".into()));
        }
        let rounding_mode = self.opt_string(
            options,
            "roundingMode",
            "halfExpand",
            &[
                "ceil", "floor", "expand", "trunc", "halfCeil", "halfFloor", "halfExpand",
                "halfTrunc", "halfEven",
            ],
        )?;
        let rounding_priority = self.opt_string(
            options,
            "roundingPriority",
            "auto",
            &["auto", "morePrecision", "lessPrecision"],
        )?;
        let trailing_zero_display =
            self.opt_string(options, "trailingZeroDisplay", "auto", &["auto", "stripIfInteger"])?;
        let has_sd = mnsd_raw != Value::UNDEFINED || mxsd_raw != Value::UNDEFINED;
        let has_fd = mnfd_raw != Value::UNDEFINED || mxfd_raw != Value::UNDEFINED;
        let (mut need_sd, mut need_fd) = (true, true);
        if rounding_priority == "auto" {
            need_sd = has_sd;
            if has_sd || (!has_fd && notation == "compact") {
                need_fd = false;
            }
        }
        let mut out = DigitOptions {
            min_integer: mnid,
            min_fraction: None,
            max_fraction: None,
            min_significant: None,
            max_significant: None,
            rounding_increment,
            rounding_mode,
            rounding_priority,
            trailing_zero_display,
        };
        if need_sd {
            if has_sd {
                let mnsd = self.default_number_option(mnsd_raw, "minimumSignificantDigits", 1, 21)?
                    .unwrap_or(1);
                let mxsd = self
                    .default_number_option(mxsd_raw, "maximumSignificantDigits", mnsd, 21)?
                    .unwrap_or(21);
                out.min_significant = Some(mnsd);
                out.max_significant = Some(mxsd);
            } else {
                out.min_significant = Some(1);
                out.max_significant = Some(21);
            }
        }
        if need_fd {
            if has_fd {
                let mnfd = self.default_number_option(mnfd_raw, "minimumFractionDigits", 0, 100)?;
                let mxfd = self.default_number_option(mxfd_raw, "maximumFractionDigits", 0, 100)?;
                let (mnfd, mxfd) = match (mnfd, mxfd) {
                    (None, Some(x)) => (mnfd_default.min(x), x),
                    (Some(n), None) => (n, mxfd_default.max(n)),
                    (Some(n), Some(x)) => {
                        if n > x {
                            return Err(Thrown(
                                "RangeError: minimumFractionDigits is greater than maximumFractionDigits"
                                    .into(),
                            ));
                        }
                        (n, x)
                    }
                    (None, None) => (mnfd_default, mxfd_default),
                };
                out.min_fraction = Some(mnfd);
                out.max_fraction = Some(mxfd);
            } else {
                out.min_fraction = Some(mnfd_default);
                out.max_fraction = Some(mxfd_default);
            }
        }
        if !need_sd && !need_fd {
            // Compact notation with no digit options at all: the "morePrecision"
            // pairing of 2 significant digits against 0 fraction digits.
            out.min_fraction = Some(0);
            out.max_fraction = Some(0);
            out.min_significant = Some(1);
            out.max_significant = Some(2);
            out.rounding_increment = 1;
        } else if rounding_increment != 1 {
            // Step 28: a rounding increment only makes sense against a fixed
            // fraction-digit count.
            if out.min_significant.is_some() && has_sd {
                return Err(Thrown(
                    "TypeError: roundingIncrement cannot be mixed with significant digits".into(),
                ));
            }
            if out.min_fraction != out.max_fraction {
                return Err(Thrown(
                    "RangeError: roundingIncrement requires equal min/max fraction digits".into(),
                ));
            }
        }
        Ok(out)
    }

    /// Write the resolved digit slots into a resolvedOptions map, in the order the
    /// ECMA-402 table gives them (integer, fraction pair, significant pair).
    fn store_digit_options(&mut self, r: &mut ObjMap, d: &DigitOptions) {
        r.set("minimumIntegerDigits", Value::num(d.min_integer as f64));
        if let (Some(n), Some(x)) = (d.min_fraction, d.max_fraction) {
            r.set("minimumFractionDigits", Value::num(n as f64));
            r.set("maximumFractionDigits", Value::num(x as f64));
        }
        if let (Some(n), Some(x)) = (d.min_significant, d.max_significant) {
            r.set("minimumSignificantDigits", Value::num(n as f64));
            r.set("maximumSignificantDigits", Value::num(x as f64));
        }
    }

    /// GetBooleanOrStringNumberFormatOption(options, "useGrouping", …): `true`
    /// stays the boolean `true`, anything falsy becomes `false`, and a string must
    /// be one of the three grouping strategies. resolvedOptions echoes it as-is,
    /// so the boolean/string distinction has to survive.
    fn read_use_grouping(&mut self, options: Value, fallback: &str) -> Result<UseGrouping, Thrown> {
        if options == Value::UNDEFINED {
            return Ok(UseGrouping::Str(fallback.to_string()));
        }
        let v = self.get_prop(options, "useGrouping")?;
        if v == Value::UNDEFINED {
            return Ok(UseGrouping::Str(fallback.to_string()));
        }
        if v == Value::bool(true) {
            return Ok(UseGrouping::Bool(true));
        }
        if !self.truthy(v) {
            return Ok(UseGrouping::Bool(false));
        }
        let s = self.to_js_string(v)?;
        if !["min2", "auto", "always"].contains(&s.as_str()) {
            return Err(Thrown(format!("RangeError: invalid useGrouping value: {s}")));
        }
        Ok(UseGrouping::Str(s))
    }

    /// `new Intl.<service>(locales, options)` → build resolved options + instance.
    pub(crate) fn make_intl(&mut self, kind: u8, locales: Value, options: Value) -> Result<Value, Thrown> {
        use native::*;
        if kind == INTL_LOCALE {
            if options != Value::UNDEFINED && !self.is_object_value(options) {
                return Err(Thrown("TypeError: Options must be an object or undefined".into()));
            }
            return self.make_locale(locales, options);
        }
        // Step 1 of every Initialize<Service> is CanonicalizeLocaleList(locales) —
        // it runs BEFORE the options argument is touched, so a bad locale tag wins
        // over a bad options bag.
        let requested = self.canonicalize_locale_list(locales)?;
        // Step 2. The four services that predate GetOptionsObject use
        // CoerceOptionsToObject instead: a primitive options argument is WRAPPED
        // (`new Intl.NumberFormat("en", "foo")` is legal), and only null throws.
        // Everything newer rejects any non-object outright.
        let options = match kind {
            INTL_NUMBERFORMAT | INTL_DATETIMEFORMAT | INTL_COLLATOR | INTL_RELATIVETIMEFORMAT => {
                if options == Value::UNDEFINED {
                    options
                } else {
                    // ToObject(null) is a TypeError; the engine's `to_object`
                    // is lenient there, so gate it explicitly.
                    self.require_object_coercible(options)?;
                    self.to_object(options)?
                }
            }
            _ => {
                if options != Value::UNDEFINED && !self.is_object_value(options) {
                    return Err(Thrown("TypeError: Options must be an object or undefined".into()));
                }
                options
            }
        };
        // Step 3. `localeMatcher` is read (and range-checked) first by every
        // service, and is deliberately NOT reflected in resolvedOptions.
        // InitializeCollator is the one exception: it reads `usage` first,
        // because the usage decides which locale data would be looked up.
        let collator_usage = if kind == INTL_COLLATOR {
            Some(self.opt_string(options, "usage", "sort", &["sort", "search"])?)
        } else {
            None
        };
        self.opt_string(options, "localeMatcher", "best fit", &["lookup", "best fit"])?;
        let locale = requested.into_iter().next().unwrap_or_else(|| "en".to_string());
        let loc = self.alloc_str(locale.clone());
        let mut r = ObjMap::new();
        r.set("locale", loc);
        match kind {
            INTL_NUMBERFORMAT => {
                // SetNumberFormatUnitOptions, then notation, then
                // SetNumberFormatDigitOptions, then compactDisplay/useGrouping/
                // signDisplay — the exact read order constructor-option-read-order
                // asserts with a property-bag observer.
                let ns = self.opt_string_opt(options, "numberingSystem", &[])?;
                if let Some(ref n) = ns {
                    if !is_well_formed_type_code(n) {
                        return Err(Thrown(format!("RangeError: invalid numberingSystem: {n}")));
                    }
                }
                let style = self.opt_string(
                    options,
                    "style",
                    "decimal",
                    &["decimal", "percent", "currency", "unit"],
                )?;
                let currency = self.opt_string_opt(options, "currency", &[])?;
                if let Some(ref c) = currency {
                    if c.len() != 3 || !c.chars().all(|ch| ch.is_ascii_alphabetic()) {
                        return Err(Thrown(format!("RangeError: invalid currency code: {c}")));
                    }
                }
                if style == "currency" && currency.is_none() {
                    return Err(Thrown(
                        "TypeError: currency must be provided for style 'currency'".into(),
                    ));
                }
                let currency_display = self.opt_string(
                    options,
                    "currencyDisplay",
                    "symbol",
                    &["code", "symbol", "narrowSymbol", "name"],
                )?;
                let currency_sign =
                    self.opt_string(options, "currencySign", "standard", &["standard", "accounting"])?;
                let unit = self.opt_string_opt(options, "unit", &[])?;
                if let Some(ref u) = unit {
                    if !is_well_formed_unit(u) {
                        return Err(Thrown(format!("RangeError: invalid unit identifier: {u}")));
                    }
                }
                if style == "unit" && unit.is_none() {
                    return Err(Thrown("TypeError: unit must be provided for style 'unit'".into()));
                }
                let unit_display =
                    self.opt_string(options, "unitDisplay", "short", &["short", "narrow", "long"])?;
                let notation = self.opt_string(
                    options,
                    "notation",
                    "standard",
                    &["standard", "scientific", "engineering", "compact"],
                )?;
                // The fraction-digit defaults depend on the style: a currency uses
                // its minor-unit count, a percent 0, anything else 0..3.
                let (mnfd_def, mxfd_def) = match style.as_str() {
                    "currency" => {
                        let d = currency_digits(currency.as_deref().unwrap_or("USD"));
                        (d, d)
                    }
                    "percent" => (0, 0),
                    _ => (0, 3),
                };
                let digits =
                    self.read_number_format_digit_options(options, mnfd_def, mxfd_def, &notation)?;
                let compact_display =
                    self.opt_string(options, "compactDisplay", "short", &["short", "long"])?;
                let default_grouping = if notation == "compact" { "min2" } else { "auto" };
                let use_grouping = self.read_use_grouping(options, default_grouping)?;
                let sign_display = self.opt_string(
                    options,
                    "signDisplay",
                    "auto",
                    &["auto", "never", "always", "exceptZero", "negative"],
                )?;
                // resolvedOptions key order is fixed by the ECMA-402 table, and is
                // NOT the read order above.
                let nsv = self.alloc_str(resolve_available(
                    ns.or_else(|| unicode_ext_value(&locale, "nu")),
                    AVAILABLE_NUMBERING_SYSTEMS,
                    "latn",
                ));
                r.set("numberingSystem", nsv);
                let sv = self.alloc_str(style.clone());
                r.set("style", sv);
                if style == "currency" {
                    let cv = self.alloc_str(currency.clone().unwrap_or_default().to_uppercase());
                    r.set("currency", cv);
                    let cdv = self.alloc_str(currency_display);
                    r.set("currencyDisplay", cdv);
                    let csv = self.alloc_str(currency_sign);
                    r.set("currencySign", csv);
                }
                if style == "unit" {
                    let uv = self.alloc_str(unit.clone().unwrap_or_default());
                    r.set("unit", uv);
                    let udv = self.alloc_str(unit_display);
                    r.set("unitDisplay", udv);
                }
                self.store_digit_options(&mut r, &digits);
                match use_grouping {
                    UseGrouping::Bool(b) => {
                        r.set("useGrouping", Value::bool(b));
                    }
                    UseGrouping::Str(ref s) => {
                        let v = self.alloc_str(s.clone());
                        r.set("useGrouping", v);
                    }
                }
                let nv = self.alloc_str(notation.clone());
                r.set("notation", nv);
                if notation == "compact" {
                    let cv = self.alloc_str(compact_display);
                    r.set("compactDisplay", cv);
                }
                let sdv = self.alloc_str(sign_display);
                r.set("signDisplay", sdv);
                let rounding = self.alloc_str(digits.rounding_mode.clone());
                r.set("roundingIncrement", Value::num(digits.rounding_increment as f64));
                r.set("roundingMode", rounding);
                let rp = self.alloc_str(digits.rounding_priority.clone());
                r.set("roundingPriority", rp);
                let tzd = self.alloc_str(digits.trailing_zero_display.clone());
                r.set("trailingZeroDisplay", tzd);
            }
            INTL_DATETIMEFORMAT => {
                // CreateDateTimeFormat reads in this order: calendar,
                // numberingSystem, hour12, hourCycle, timeZone, the twelve
                // components, formatMatcher, dateStyle, timeStyle.
                let cal_opt = self.opt_string_opt(options, "calendar", &[])?;
                if let Some(ref c) = cal_opt {
                    if !is_well_formed_type_code(c) {
                        return Err(Thrown(format!("RangeError: invalid calendar: {c}")));
                    }
                }
                let ns_opt = self.opt_string_opt(options, "numberingSystem", &[])?;
                if let Some(ref n) = ns_opt {
                    if !is_well_formed_type_code(n) {
                        return Err(Thrown(format!("RangeError: invalid numberingSystem: {n}")));
                    }
                }
                let hour12 = self.opt_bool_opt(options, "hour12")?;
                let hour_cycle_opt =
                    self.opt_string_opt(options, "hourCycle", &["h11", "h12", "h23", "h24"])?;
                // A PRESENT hour12 overrides any hourCycle (step 14), even `false`.
                let hour_cycle_opt = if hour12.is_some() { None } else { hour_cycle_opt };
                let tz_v = if options == Value::UNDEFINED {
                    Value::UNDEFINED
                } else {
                    self.get_prop(options, "timeZone")?
                };
                let tz = if tz_v == Value::UNDEFINED {
                    "UTC".to_string()
                } else {
                    let s = self.to_js_string(tz_v)?;
                    match canonicalize_time_zone(&s) {
                        Some(c) => c,
                        None => {
                            return Err(Thrown(format!("RangeError: invalid time zone: {s}")))
                        }
                    }
                };
                let comps: [(&str, &[&str]); 10] = [
                    ("weekday", &["narrow", "short", "long"]),
                    ("era", &["narrow", "short", "long"]),
                    ("year", &["2-digit", "numeric"]),
                    ("month", &["2-digit", "numeric", "narrow", "short", "long"]),
                    ("day", &["2-digit", "numeric"]),
                    ("dayPeriod", &["narrow", "short", "long"]),
                    ("hour", &["2-digit", "numeric"]),
                    ("minute", &["2-digit", "numeric"]),
                    ("second", &["2-digit", "numeric"]),
                    ("timeZoneName", &[
                        "short", "long", "shortOffset", "longOffset", "shortGeneric",
                        "longGeneric",
                    ]),
                ];
                let mut vals: Vec<(&str, String)> = vec![];
                let mut frac_digits: Option<i64> = None;
                for (name, allowed) in comps {
                    // fractionalSecondDigits is a NUMBER option sitting between
                    // `second` and `timeZoneName` in the read order.
                    if name == "timeZoneName" {
                        frac_digits = self.opt_int_opt(options, "fractionalSecondDigits", 1, 3)?;
                    }
                    if let Some(v) = self.opt_string_opt(options, name, allowed)? {
                        vals.push((name, v));
                    }
                }
                let _ = self.opt_string(options, "formatMatcher", "best fit", &["basic", "best fit"])?;
                let date_style =
                    self.opt_string_opt(options, "dateStyle", &["full", "long", "medium", "short"])?;
                let time_style =
                    self.opt_string_opt(options, "timeStyle", &["full", "long", "medium", "short"])?;
                // Step 41: a style and an explicit component cannot be combined.
                if (date_style.is_some() || time_style.is_some())
                    && (!vals.is_empty() || frac_digits.is_some())
                {
                    return Err(Thrown(
                        "TypeError: dateStyle/timeStyle may not be used with explicit date-time components"
                            .into(),
                    ));
                }
                // No components and no style at all → the "any date, all" default
                // pattern (year/month/day).
                if vals.is_empty() && frac_digits.is_none() && date_style.is_none()
                    && time_style.is_none()
                {
                    vals = vec![
                        ("year", "numeric".to_string()),
                        ("month", "numeric".to_string()),
                        ("day", "numeric".to_string()),
                    ];
                }
                // The -u-ca / -u-nu extension keywords lose to an explicit option.
                let ext = |k: &str| unicode_ext_value(&locale, k);
                let calv = self.alloc_str(resolve_available(
                    cal_opt.or_else(|| ext("ca")),
                    AVAILABLE_CALENDARS,
                    "gregory",
                ));
                r.set("calendar", calv);
                let nsv = self.alloc_str(resolve_available(
                    ns_opt.or_else(|| ext("nu")),
                    AVAILABLE_NUMBERING_SYSTEMS,
                    "latn",
                ));
                r.set("numberingSystem", nsv);
                let tzv = self.alloc_str(tz);
                r.set("timeZone", tzv);
                // hourCycle/hour12 are reported only when the resolved pattern has
                // an hour field — an explicit `hour` component or any timeStyle.
                let has_hour = vals.iter().any(|(n, _)| *n == "hour") || time_style.is_some();
                if has_hour {
                    let hc = match (hour12, hour_cycle_opt.clone()) {
                        // hour12:false is h23; hour12:true keeps the locale's
                        // 12-hour cycle (h12 for the en-style default).
                        (Some(false), _) => "h23".to_string(),
                        (Some(true), _) => "h12".to_string(),
                        (None, Some(h)) => h,
                        (None, None) => ext("hc").unwrap_or_else(|| "h12".to_string()),
                    };
                    let is12 = hc == "h11" || hc == "h12";
                    let hcv = self.alloc_str(hc);
                    r.set("hourCycle", hcv);
                    r.set("hour12", Value::bool(is12));
                }
                // `vals` is already in the resolvedOptions table order (it was
                // filled by walking `comps`); fractionalSecondDigits belongs
                // between `second` and `timeZoneName`.
                let mut frac_emitted = false;
                for (name, v) in vals {
                    if name == "timeZoneName" && !frac_emitted {
                        if let Some(f) = frac_digits {
                            r.set("fractionalSecondDigits", Value::num(f as f64));
                        }
                        frac_emitted = true;
                    }
                    let vv = self.alloc_str(v);
                    r.set(name, vv);
                }
                if !frac_emitted {
                    if let Some(f) = frac_digits {
                        r.set("fractionalSecondDigits", Value::num(f as f64));
                    }
                }
                if let Some(s) = date_style {
                    let v = self.alloc_str(s);
                    r.set("dateStyle", v);
                }
                if let Some(s) = time_style {
                    let v = self.alloc_str(s);
                    r.set("timeStyle", v);
                }
            }
            INTL_COLLATOR => {
                // Read order after `usage`/`localeMatcher`: collation, numeric,
                // caseFirst, sensitivity, ignorePunctuation. `collation` and
                // `ignorePunctuation` were not read at all before, so their
                // throwing getters never ran and `{ignorePunctuation: true}` was
                // silently ignored.
                let collation = self.opt_string_opt(options, "collation", &[])?;
                if let Some(ref c) = collation {
                    if !is_well_formed_type_code(c) {
                        return Err(Thrown(format!("RangeError: invalid collation: {c}")));
                    }
                }
                let numeric = self.opt_bool_opt(options, "numeric")?;
                let case_first =
                    self.opt_string_opt(options, "caseFirst", &["upper", "lower", "false"])?;
                let sens = self.opt_string(
                    options,
                    "sensitivity",
                    "variant",
                    &["base", "accent", "case", "variant"],
                )?;
                let ignore_punct = self.opt_bool_opt(options, "ignorePunctuation")?;
                // resolvedOptions table order: locale, usage, sensitivity,
                // ignorePunctuation, collation, numeric, caseFirst.
                let uv = self.alloc_str(collator_usage.unwrap_or_else(|| "sort".to_string()));
                r.set("usage", uv);
                let sv = self.alloc_str(sens);
                r.set("sensitivity", sv);
                // No CLDR ignorePunctuation defaults (Thai/Lao want `true`), so
                // an unset option resolves to `false` — the root behaviour.
                r.set("ignorePunctuation", Value::bool(ignore_punct.unwrap_or(false)));
                // Only the root collation is implemented, so a requested `-u-co-`
                // / `collation` value that is not it resolves to "default"
                // rather than being echoed back as if it were honoured.
                let requested = collation.or_else(|| unicode_ext_value(&locale, "co"));
                let col = self.alloc_str(match requested.as_deref() {
                    // "standard"/"search" are never valid as a resolved collation.
                    Some("standard") | Some("search") | None => "default".to_string(),
                    Some(_) => "default".to_string(),
                });
                r.set("collation", col);
                // `-u-kn` with no value IS the canonical spelling of `-u-kn-true`,
                // so key presence (not a "true" value) is what turns it on.
                let kn = numeric.unwrap_or_else(|| {
                    unicode_ext_has_key(&locale, "kn")
                        && unicode_ext_value(&locale, "kn").as_deref() != Some("false")
                });
                r.set("numeric", Value::bool(kn));
                let kf = case_first
                    .or_else(|| unicode_ext_value(&locale, "kf"))
                    .filter(|s| ["upper", "lower", "false"].contains(&s.as_str()))
                    .unwrap_or_else(|| "false".to_string());
                let cfv = self.alloc_str(kf);
                r.set("caseFirst", cfv);
            }
            INTL_PLURALRULES => {
                // InitializePluralRules: type, notation, compactDisplay, then the
                // shared SetNumberFormatDigitOptions block (0..3 fraction default).
                let t = self.opt_string(options, "type", "cardinal", &["cardinal", "ordinal"])?;
                let notation = self.opt_string(
                    options,
                    "notation",
                    "standard",
                    &["standard", "scientific", "engineering", "compact"],
                )?;
                let compact_display =
                    self.opt_string(options, "compactDisplay", "short", &["short", "long"])?;
                let digits = self.read_number_format_digit_options(options, 0, 3, &notation)?;
                let tv = self.alloc_str(t);
                r.set("type", tv);
                // resolvedOptions table order: type, notation, then the digit
                // block. `notation` was resolved but never reported.
                let nv = self.alloc_str(notation.clone());
                r.set("notation", nv);
                if notation == "compact" {
                    let cv = self.alloc_str(compact_display);
                    r.set("compactDisplay", cv);
                }
                self.store_digit_options(&mut r, &digits);
                // PluralRules has no `numberingSystem` in its resolvedOptions table.
                let cats = ["one", "other"]
                    .iter()
                    .map(|c| self.alloc_str(c.to_string()))
                    .collect::<Vec<_>>();
                let arr = Value::heap(self.heap.alloc(HeapObj::Array(cats)));
                r.set("pluralCategories", arr);
                let rm = self.alloc_str(digits.rounding_mode.clone());
                r.set("roundingIncrement", Value::num(digits.rounding_increment as f64));
                r.set("roundingMode", rm);
                let rp = self.alloc_str(digits.rounding_priority.clone());
                r.set("roundingPriority", rp);
                let tzd = self.alloc_str(digits.trailing_zero_display.clone());
                r.set("trailingZeroDisplay", tzd);
            }
            INTL_LISTFORMAT => {
                let t =
                    self.opt_string(options, "type", "conjunction", &["conjunction", "disjunction", "unit"])?;
                let tv = self.alloc_str(t);
                r.set("type", tv);
                let st =
                    self.opt_string(options, "style", "long", &["long", "short", "narrow"])?;
                let sv = self.alloc_str(st);
                r.set("style", sv);
            }
            INTL_RELATIVETIMEFORMAT => {
                // Read order: localeMatcher (above), numberingSystem, style,
                // numeric — the sequence `constructor/options-order` asserts.
                // `numberingSystem` was never read, so its throwing getter never
                // ran and an ill-formed value never threw.
                let ns_opt = self.opt_string_opt(options, "numberingSystem", &[])?;
                if let Some(ref n) = ns_opt {
                    if !is_well_formed_type_code(n) {
                        return Err(Thrown(format!("RangeError: invalid numberingSystem: {n}")));
                    }
                }
                let st = self.opt_string(options, "style", "long", &["long", "short", "narrow"])?;
                let sv = self.alloc_str(st);
                r.set("style", sv);
                let nm = self.opt_string(options, "numeric", "always", &["always", "auto"])?;
                let nmv = self.alloc_str(nm);
                r.set("numeric", nmv);
                let ns = self.alloc_str(resolve_available(
                    ns_opt.or_else(|| unicode_ext_value(&locale, "nu")),
                    AVAILABLE_NUMBERING_SYSTEMS,
                    "latn",
                ));
                r.set("numberingSystem", ns);
            }
            INTL_SEGMENTER => {
                let g =
                    self.opt_string(options, "granularity", "grapheme", &["grapheme", "word", "sentence"])?;
                let gv = self.alloc_str(g);
                r.set("granularity", gv);
            }
            INTL_DISPLAYNAMES => {
                // Read order: style, type, fallback, languageDisplay. `style`
                // comes FIRST — an invalid style must RangeError even though the
                // (required) `type` is what a missing options bag trips on.
                let st = self.opt_string(options, "style", "long", &["long", "short", "narrow"])?;
                let t = self.opt_string_opt(
                    options,
                    "type",
                    &["language", "region", "script", "currency", "calendar", "dateTimeField"],
                )?;
                let t = t.ok_or_else(|| {
                    Thrown("TypeError: Intl.DisplayNames type option is required".into())
                })?;
                let fb = self.opt_string(options, "fallback", "code", &["code", "none"])?;
                let ld =
                    self.opt_string(options, "languageDisplay", "dialect", &["dialect", "standard"])?;
                // resolvedOptions table order: locale, style, type, fallback,
                // languageDisplay (the last only for type "language").
                let sv = self.alloc_str(st);
                r.set("style", sv);
                let is_language = t == "language";
                let tv = self.alloc_str(t);
                r.set("type", tv);
                let fbv = self.alloc_str(fb);
                r.set("fallback", fbv);
                if is_language {
                    let ldv = self.alloc_str(ld);
                    r.set("languageDisplay", ldv);
                }
            }
            INTL_DURATIONFORMAT => {
                // Read order: localeMatcher (above), numberingSystem, style, then
                // the ten (unit, unitDisplay) pairs in table order, then
                // fractionalDigits — asserted by constructor-options-order.
                let ns_opt = self.opt_string_opt(options, "numberingSystem", &[])?;
                if let Some(ref n) = ns_opt {
                    if !is_well_formed_type_code(n) {
                        return Err(Thrown(format!("RangeError: invalid numberingSystem: {n}")));
                    }
                }
                let st =
                    self.opt_string(options, "style", "short", &["long", "short", "narrow", "digital"])?;
                // GetUnitOptions (ECMA-402 Table 1). `values` widens for the time
                // units, and `digital_base` is the style a "digital" duration
                // gives that unit.
                const UNITS: [(&str, bool, bool); 10] = [
                    // (unit, allows numeric/2-digit, allows 2-digit)
                    ("years", false, false),
                    ("months", false, false),
                    ("weeks", false, false),
                    ("days", false, false),
                    ("hours", true, true),
                    ("minutes", true, true),
                    ("seconds", true, true),
                    ("milliseconds", true, false),
                    ("microseconds", true, false),
                    ("nanoseconds", true, false),
                ];
                let mut resolved_units: Vec<(String, String)> = vec![];
                let mut prev_style = String::new();
                for (unit, numeric_ok, two_digit_ok) in UNITS {
                    let mut allowed: Vec<&str> = vec!["long", "short", "narrow"];
                    if numeric_ok {
                        allowed.push("numeric");
                    }
                    if two_digit_ok {
                        allowed.push("2-digit");
                    }
                    let requested = self.opt_string_opt(options, unit, &allowed)?;
                    // Step 6: once a unit is numeric-like, every later time unit
                    // must be too — a "long" after a "numeric" is a RangeError.
                    if let Some(ref s) = requested {
                        if matches!(prev_style.as_str(), "numeric" | "2-digit")
                            && !matches!(s.as_str(), "numeric" | "2-digit")
                        {
                            return Err(Thrown(format!(
                                "RangeError: {unit} style {s} cannot follow a numeric unit"
                            )));
                        }
                    }
                    // GetUnitOptions' display default is "always" unless a rule
                    // below relaxes it — an EXPLICIT style therefore keeps
                    // "always" (`{hours:"numeric"}` ⇒ hoursDisplay "always").
                    let mut display_default = "always";
                    let is_digital_core = matches!(unit, "hours" | "minutes" | "seconds");
                    let style = match requested {
                        Some(s) => s,
                        None if st == "digital" => {
                            if !is_digital_core {
                                display_default = "auto";
                            }
                            if numeric_ok { "numeric".to_string() } else { "short".to_string() }
                        }
                        None if matches!(prev_style.as_str(), "fractional" | "numeric" | "2-digit") => {
                            // Step 3.b.i: after a numeric-like unit every later
                            // time unit is numeric too, and only minutes/seconds
                            // stay "always".
                            if !matches!(unit, "minutes" | "seconds") {
                                display_default = "auto";
                            }
                            "numeric".to_string()
                        }
                        None => {
                            display_default = "auto";
                            if st == "long" || st == "narrow" { st.clone() } else { "short".to_string() }
                        }
                    };
                    // Step 8.b: minutes/seconds following a numeric-like unit
                    // render as "2-digit".
                    let style = if matches!(prev_style.as_str(), "numeric" | "2-digit")
                        && matches!(unit, "minutes" | "seconds")
                        && matches!(style.as_str(), "numeric" | "2-digit")
                    {
                        "2-digit".to_string()
                    } else {
                        style
                    };
                    // Step 4: a "numeric" sub-second unit is really a fraction of
                    // the second before it, so it is only shown when non-zero.
                    if style == "numeric" && !two_digit_ok && numeric_ok {
                        display_default = "auto";
                    }
                    let disp_key = format!("{unit}Display");
                    let display =
                        self.opt_string(options, &disp_key, display_default, &["auto", "always"])?;
                    prev_style = style.clone();
                    resolved_units.push((style, display));
                }
                let frac = self.opt_int_opt(options, "fractionalDigits", 0, 9)?;
                // resolvedOptions table order: locale, numberingSystem, style,
                // then each unit followed by its display, then fractionalDigits.
                let nsv = self.alloc_str(resolve_available(
                    ns_opt.or_else(|| unicode_ext_value(&locale, "nu")),
                    AVAILABLE_NUMBERING_SYSTEMS,
                    "latn",
                ));
                r.set("numberingSystem", nsv);
                let sv = self.alloc_str(st);
                r.set("style", sv);
                for ((unit, _, _), (style, display)) in UNITS.iter().zip(resolved_units) {
                    let s = self.alloc_str(style);
                    r.set(unit, s);
                    let d = self.alloc_str(display);
                    r.set(&format!("{unit}Display"), d);
                }
                if let Some(f) = frac {
                    r.set("fractionalDigits", Value::num(f as f64));
                }
            }
            _ => {}
        }
        let resolved = self.heap.alloc(HeapObj::Object(Box::new(r)));
        let idx = self.heap.alloc(HeapObj::Intl { kind, resolved });
        if self.intl_protos[kind as usize] != 0 {
            self.proto_of.insert(idx, Value::heap(self.intl_protos[kind as usize]));
        }
        Ok(Value::heap(idx))
    }

    /// `new Intl.Locale(tag, options)` (ECMA-402 14.1.1).
    ///
    /// The tag is parsed structurally (`locale_tag.rs`), then `UpdateLanguageId`
    /// replaces language/script/region/variants from `options`, then
    /// `ApplyUnicodeExtensionToTag` folds calendar/collation/firstDayOfWeek/
    /// hourCycle/caseFirst/numeric/numberingSystem into the `-u-` extension. The
    /// option READ order is the one `constructor-getter-order` asserts:
    /// the four language-id parts, then the extension keys in `-u-` key order
    /// (ca, co, fw, hc, kf, kn, nu).
    pub(crate) fn make_locale(&mut self, tag: Value, options: Value) -> Result<Value, Thrown> {
        use crate::vm::locale_tag as lt;
        let base = match self.locale_slot_tag(tag) {
            Some(t) => t,
            None => {
                // Step 7: a non-Locale, non-Object tag is a TypeError BEFORE any
                // ToString — but an ordinary object is ToString'd (its toString
                // runs, and its exception must win over the options reads).
                if !(tag.is_heap() && self.heap.is_str_like(tag.heap_index()))
                    && !self.is_object_value(tag)
                {
                    return Err(Thrown("TypeError: Locale tag must be a string or an object".into()));
                }
                self.to_js_string(tag)?
            }
        };
        let mut t = lt::parse_lang_tag(&base)
            .ok_or_else(|| Thrown(format!("RangeError: invalid language tag: {base}")))?;
        // ── UpdateLanguageId ──
        if let Some(l) = self.opt_string_opt(options, "language", &[])? {
            if !lt::is_language_subtag(&l) {
                return Err(Thrown(format!("RangeError: invalid language option: {l}")));
            }
            t.language = l.to_ascii_lowercase();
        }
        if let Some(s) = self.opt_string_opt(options, "script", &[])? {
            if !lt::is_script_subtag(&s) {
                return Err(Thrown(format!("RangeError: invalid script option: {s}")));
            }
            let l = s.to_ascii_lowercase();
            t.script = format!("{}{}", l[..1].to_ascii_uppercase(), &l[1..]);
        }
        if let Some(rg) = self.opt_string_opt(options, "region", &[])? {
            if !lt::is_region_subtag(&rg) {
                return Err(Thrown(format!("RangeError: invalid region option: {rg}")));
            }
            t.region = if rg.bytes().all(|b| b.is_ascii_digit()) {
                rg.clone()
            } else {
                rg.to_ascii_uppercase()
            };
        }
        if let Some(v) = self.opt_string_opt(options, "variants", &[])? {
            // `variants` is the whole "-"-joined run: every subtag must match
            // unicode_variant_subtag and none may repeat.
            let mut seen: Vec<String> = vec![];
            for part in v.split('-') {
                let lp = part.to_ascii_lowercase();
                if !lt::is_variant_subtag(&lp) || seen.contains(&lp) {
                    return Err(Thrown(format!("RangeError: invalid variants option: {v}")));
                }
                seen.push(lp);
            }
            seen.sort();
            t.variants = seen;
        }
        // ── ApplyUnicodeExtensionToTag: the relevant extension keys, in key order ──
        for (opt_name, key) in
            [("calendar", "ca"), ("collation", "co"), ("firstDayOfWeek", "fw"), ("hourCycle", "hc")]
        {
            let raw = if key == "fw" {
                // firstDayOfWeek also accepts the numeric weekday spellings
                // (and `true`, which ToString's to "true" → the bare `-u-fw-`).
                self.opt_string_opt(options, opt_name, &[])?.map(|s| lt::weekday_to_string(&s))
            } else {
                self.opt_string_opt(options, opt_name, &[])?
            };
            if let Some(v) = raw {
                if key == "hc" && !["h11", "h12", "h23", "h24"].contains(&v.as_str()) {
                    return Err(Thrown(format!("RangeError: invalid hourCycle option: {v}")));
                }
                if key != "hc" && !lt::is_type_sequence(&v) {
                    return Err(Thrown(format!("RangeError: invalid {opt_name} option: {v}")));
                }
                t.set_u(key, Some(canon_ext_value(&v)));
            }
        }
        if let Some(cf) = self.opt_string_opt(options, "caseFirst", &["upper", "lower", "false"])? {
            t.set_u("kf", Some(canon_ext_value(&cf)));
        }
        // `numeric` is a BOOLEAN option: only its presence matters, and `true`
        // canonicalizes to the bare `-u-kn`.
        if let Some(n) = self.opt_bool_opt(options, "numeric")? {
            t.set_u("kn", Some(if n { String::new() } else { "false".to_string() }));
        }
        if let Some(ns) = self.opt_string_opt(options, "numberingSystem", &[])? {
            if !lt::is_type_sequence(&ns) {
                return Err(Thrown(format!("RangeError: invalid numberingSystem option: {ns}")));
            }
            t.set_u("nu", Some(canon_ext_value(&ns)));
        }
        Ok(self.alloc_locale(&t))
    }

    /// Materialize a parsed tag as an `Intl.Locale` instance. The `resolved`
    /// object doubles as the accessor backing store, so every getter's value is
    /// stored here under its own property name; `@@tag` (hidden) is `[[Locale]]`.
    pub(crate) fn alloc_locale(&mut self, t: &crate::vm::locale_tag::LangTag) -> Value {
        use crate::vm::locale_tag as lt;
        let mut r = ObjMap::new();
        let full = t.canonical();
        let tv = self.alloc_str(full);
        r.set("@@tag", tv);
        let bn = self.alloc_str(t.base_name());
        r.set("baseName", bn);
        let lv = self.alloc_str(t.language.clone());
        r.set("language", lv);
        for (key, val) in
            [("script", t.script.clone()), ("region", t.region.clone())]
        {
            let v = if val.is_empty() { Value::UNDEFINED } else { self.alloc_str(val) };
            r.set(key, v);
        }
        let variants = if t.variants.is_empty() {
            Value::UNDEFINED
        } else {
            self.alloc_str(t.variants.join("-"))
        };
        r.set("variants", variants);
        for (key, uk) in [
            ("calendar", "ca"),
            ("collation", "co"),
            ("firstDayOfWeek", "fw"),
            ("hourCycle", "hc"),
        ] {
            // A bare `-u-ca` (no value) is not a resolved value: the getter
            // reports undefined, exactly as if the key were absent.
            let v = match t.u_value(uk) {
                Some(s) if !s.is_empty() => self.alloc_str(s.to_string()),
                _ => Value::UNDEFINED,
            };
            r.set(key, v);
        }
        // `-u-kf-true` canonicalizes to a bare `-u-kf`, whose caseFirst is "".
        let cf = match t.u_value("kf") {
            Some(s) => self.alloc_str(s.to_string()),
            None => Value::UNDEFINED,
        };
        r.set("caseFirst", cf);
        // `-u-kn` (bare) and `-u-kn-true` are both `numeric === true`.
        r.set("numeric", Value::bool(matches!(t.u_value("kn"), Some(""))));
        let ns = match t.u_value("nu") {
            Some(s) if !s.is_empty() => self.alloc_str(s.to_string()),
            _ => Value::UNDEFINED,
        };
        r.set("numberingSystem", ns);
        // The 1..7 weekday `getWeekInfo` reports, when `-u-fw-` names one.
        let fw_idx = t.u_value("fw").and_then(lt::weekday_index);
        r.set("@@fwindex", match fw_idx {
            Some(i) => Value::num(i as f64),
            None => Value::UNDEFINED,
        });
        let resolved = self.heap.alloc(HeapObj::Object(Box::new(r)));
        let idx = self.heap.alloc(HeapObj::Intl { kind: native::INTL_LOCALE, resolved });
        if self.intl_protos[native::INTL_LOCALE as usize] != 0 {
            self.proto_of.insert(idx, Value::heap(self.intl_protos[native::INTL_LOCALE as usize]));
        }
        Value::heap(idx)
    }

    /// StringListFromIterable (ECMA-402 13.5.1) — Intl.ListFormat's argument
    /// coercion. `undefined` is an empty list (NOT a TypeError), and a yielded
    /// value that is not a String stops iteration at once with a TypeError,
    /// after IteratorClose. Draining first and checking after would run the
    /// iterator past the offending element, which `iterable-iteratorclose`
    /// observes through the iterator's own step counter.
    pub(crate) fn string_list_from_iterable(&mut self, iterable: Value) -> Result<Vec<String>, Thrown> {
        if iterable == Value::UNDEFINED {
            return Ok(vec![]);
        }
        let iter = self.get_iterator(iterable)?;
        let next = if iter.is_heap()
            && matches!(
                self.heap.get(iter.heap_index()),
                HeapObj::Object(_)
                    | HeapObj::Proxy { .. }
                    | HeapObj::Iterator { .. }
                    | HeapObj::IterHelper { .. }
            ) {
            let n = self.get_prop(iter, "next")?;
            self.is_callable(n).then_some(n)
        } else {
            None
        };
        let Some(next) = next else {
            // A dense array / string / generator has no user-visible step
            // function to observe, so the shared drain is equivalent there.
            let vals = self.iterate_to_vec(iterable)?;
            let mut out = Vec::with_capacity(vals.len());
            for v in vals {
                if !(v.is_heap() && self.heap.is_str_like(v.heap_index())) {
                    return Err(Thrown("TypeError: list elements must be strings".into()));
                }
                out.push(self.heap.str_cow(v.heap_index()).unwrap().into_owned());
            }
            return Ok(out);
        };
        let mut out: Vec<String> = vec![];
        loop {
            let res = self.call_value(next, iter, &[])?;
            if !self.is_object_value(res) {
                return Err(Thrown("TypeError: iterator.next() returned a non-object".into()));
            }
            let done = self.get_prop(res, "done")?;
            if self.truthy(done) {
                break;
            }
            let v = self.get_prop(res, "value")?;
            if !(v.is_heap() && self.heap.is_str_like(v.heap_index())) {
                self.iterator_close_quiet(iter);
                return Err(Thrown("TypeError: list elements must be strings".into()));
            }
            out.push(self.heap.str_cow(v.heap_index()).unwrap().into_owned());
        }
        Ok(out)
    }

    /// The Intl.Locale-info methods (`getCalendars` … `getWeekInfo`).
    ///
    /// ECMA-402 sources these from CLDR supplemental data, which this engine does
    /// not ship. Rather than invent per-locale answers, each list reports what
    /// **this engine actually implements** — the same sets `Intl.supportedValuesOf`
    /// and the DateTimeFormat/Collator resolvedOptions surfaces report, so the
    /// three agree — and the two purely structural rules (a region-less tag has no
    /// time zones; `-u-fw-` overrides the first day of the week) are honoured
    /// exactly.
    pub(crate) fn locale_info(&mut self, resolved: u32, which: &str) -> Result<Value, Thrown> {
        let tag = self.display(self.intl_slot(resolved, "@@tag"));
        let strings = |vm: &mut Self, xs: &[&str]| -> Value {
            let items: Vec<Value> = xs.iter().map(|s| vm.alloc_str(s.to_string())).collect();
            Value::heap(vm.heap.alloc(HeapObj::Array(items)))
        };
        Ok(match which {
            // CreateArrayFromListAndPreferred: a `-u-ca-` request that this
            // engine supports leads the list.
            "getCalendars" => {
                let pref = self.display(self.intl_slot(resolved, "calendar")).to_ascii_lowercase();
                let mut list: Vec<&str> = AVAILABLE_CALENDARS.to_vec();
                if let Some(p) = list.iter().position(|c| *c == pref) {
                    let c = list.remove(p);
                    list.insert(0, c);
                }
                strings(self, &list)
            }
            "getNumberingSystems" => strings(self, AVAILABLE_NUMBERING_SYSTEMS),
            // One collation, the one Collator.resolvedOptions() names. "standard"
            // and "search" are excluded from this list by the spec.
            "getCollations" => strings(self, &["default"]),
            // The hour cycle DateTimeFormat resolves when nothing overrides it.
            "getHourCycles" => strings(self, &["h12"]),
            "getTimeZones" => {
                // Structural rule: no region subtag → undefined, no data needed.
                if self.intl_slot(resolved, "region") == Value::UNDEFINED {
                    Value::UNDEFINED
                } else {
                    // There is no tz database here; UTC is the only zone the
                    // engine implements, so it is the only one it can name.
                    strings(self, &["UTC"])
                }
            }
            "getTextInfo" => {
                let script = self.display(self.intl_slot(resolved, "script"));
                let dir = if self.intl_slot(resolved, "script") != Value::UNDEFINED
                    && is_rtl_script(&script)
                {
                    "rtl"
                } else {
                    // Without likelySubtags a bare RTL language tag ("ar") cannot
                    // be resolved to its script, so it reports "ltr" here.
                    "ltr"
                };
                let mut o = ObjMap::new();
                let d = self.alloc_str(dir.to_string());
                o.set("direction", d);
                Value::heap(self.heap.alloc(HeapObj::Object(Box::new(o))))
            }
            _ => {
                // getWeekInfo. `-u-fw-` is a structural override and is honoured;
                // otherwise the CLDR *root* week data applies (first day Monday,
                // weekend Saturday+Sunday) because there is no territory table to
                // specialise it.
                let first = match self.intl_slot(resolved, "@@fwindex") {
                    v if v.is_number() => v.as_f64(),
                    _ => 1.0,
                };
                let mut o = ObjMap::new();
                o.set("firstDay", Value::num(first));
                let weekend = Value::heap(
                    self.heap.alloc(HeapObj::Array(vec![Value::num(6.0), Value::num(7.0)])),
                );
                o.set("weekend", weekend);
                let _ = tag;
                Value::heap(self.heap.alloc(HeapObj::Object(Box::new(o))))
            }
        })
    }

    /// Intl.NumberFormat.prototype.format(value).
    pub(crate) fn intl_number_format(&mut self, resolved: u32, value: Value) -> Result<Value, Thrown> {
        let n = self.to_number(value)?;
        let s = self.intl_number_format_str(resolved, n)?;
        Ok(self.alloc_str(s))
    }

    /// The string half of format(), split out so formatToParts/formatRange can
    /// re-partition the same output instead of re-deriving it.
    pub(crate) fn intl_number_format_str(&mut self, resolved: u32, n: f64) -> Result<String, Thrown> {
        let style = self.display(self.intl_slot(resolved, "style"));
        // A digit slot is ABSENT when SetNumberFormatDigitOptions did not resolve
        // that pair (significant and fraction digits are mutually exclusive under
        // "auto"), so read them as options, not as coerced numbers.
        let slot_int = |vm: &Self, k: &str| -> Option<i64> {
            let v = vm.intl_slot(resolved, k);
            v.is_number().then(|| v.as_f64() as i64)
        };
        let ug = self.intl_slot(resolved, "useGrouping");
        let grouping = ug != Value::bool(false) && self.display(ug) != "false";
        let notation = self.display(self.intl_slot(resolved, "notation"));
        let params = NumFmtParams {
            style: &style,
            notation: &notation,
            min_int: slot_int(self, "minimumIntegerDigits").unwrap_or(1),
            min_frac: slot_int(self, "minimumFractionDigits"),
            max_frac: slot_int(self, "maximumFractionDigits"),
            min_sig: slot_int(self, "minimumSignificantDigits"),
            max_sig: slot_int(self, "maximumSignificantDigits"),
            rounding_priority: &self.display(self.intl_slot(resolved, "roundingPriority")),
            rounding_mode: &self.display(self.intl_slot(resolved, "roundingMode")),
            rounding_increment: slot_int(self, "roundingIncrement").unwrap_or(1),
            trailing_zero_display: &self.display(self.intl_slot(resolved, "trailingZeroDisplay")),
            sign_display: &self.display(self.intl_slot(resolved, "signDisplay")),
            grouping,
        };
        let s = format_number_intl(n, &params);
        Ok(if style == "currency" {
            let cur = self.display(self.intl_slot(resolved, "currency"));
            // The minus sign leads the currency symbol ("-$5.00"), so splice the
            // symbol in after any sign rather than prefixing the whole string.
            match s.strip_prefix('-') {
                Some(rest) => format!("-{}{}", currency_symbol(&cur), rest),
                None => format!("{}{}", currency_symbol(&cur), s),
            }
        } else {
            s
        })
    }

    /// HandleDateTimeValue's argument classification: `None` for an ordinary time
    /// value (a Number, a Date, anything ToNumber-able), `Some(k)` for a
    /// `Temporal.*` argument (k = the HeapObj::Temporal kind). formatRange pairs
    /// only equal classifications — mixing a Date with a PlainDate, or two
    /// different Temporal types, is a TypeError.
    pub(crate) fn dt_arg_kind(&self, v: Value) -> Option<u8> {
        if v.is_heap() {
            if let HeapObj::Temporal { kind, .. } = self.heap.get(v.heap_index()) {
                return Some(*kind);
            }
        }
        None
    }

    /// HandleDateTimeValue's calendar guard: a `Temporal.*` argument is rendered
    /// with the DateTimeFormat's own calendar, so its calendar must be either
    /// `iso8601` (which any calendar can render) or exactly the format's.
    /// Anything else is a RangeError — the formatter must not silently reinterpret
    /// a Japanese date as a Gregorian one.
    pub(crate) fn dtf_check_calendar(
        &mut self,
        resolved: u32,
        v: Value,
        name: &str,
    ) -> Result<(), Thrown> {
        // Only the calendar-BEARING Temporal types carry a calendar to clash.
        if !self.dt_arg_kind(v).is_some_and(|k| matches!(k, 1 | 3 | 5 | 6 | 7)) {
            return Ok(());
        }
        let cal = self.cal_of(v.heap_index());
        if cal == crate::vm::temporal::Cal::Iso {
            return Ok(());
        }
        let want = self.intl_slot(resolved, "calendar");
        let want = if want.is_heap() {
            self.heap.str_cow(want.heap_index()).map(|s| s.into_owned()).unwrap_or_default()
        } else {
            String::new()
        };
        if cal.id() == want {
            return Ok(());
        }
        Err(Thrown(format!(
            "RangeError: {name} cannot format a {} date with the {want} calendar",
            cal.id()
        )))
    }

    /// HandleDateTimeValue: ToNumber + TimeClip for an ordinary time value (an
    /// out-of-range or non-finite one is a RangeError, and the result is an
    /// integer so `format(-0.9)` and `format(0)` agree), or the epoch time of a
    /// `Temporal.*` argument read as a UTC wall clock. Temporal.ZonedDateTime is
    /// explicitly rejected by the spec (its own toLocaleString handles it), and a
    /// Temporal.Duration is not a date-time at all.
    pub(crate) fn dtf_time_value(&mut self, v: Value) -> Result<f64, Thrown> {
        if let Some(kind) = self.dt_arg_kind(v) {
            let idx = v.heap_index();
            let day_ms = |y: i64, m: i64, d: i64| iso_to_epoch_days(y, m, d) as f64 * 86_400_000.0;
            return match kind {
                7 => Err(Thrown(
                    "TypeError: Intl.DateTimeFormat does not support Temporal.ZonedDateTime; use toLocaleString()"
                        .into(),
                )),
                0 => Err(Thrown("TypeError: Temporal.Duration is not a date-time value".into())),
                1 => {
                    let (y, m, d) = self.plain_date_fields(idx).unwrap_or((1970, 1, 1));
                    Ok(day_ms(y, m, d))
                }
                2 => {
                    let f = self.plain_time_fields(idx).unwrap_or([0; 6]);
                    Ok((f[0] * 3_600_000 + f[1] * 60_000 + f[2] * 1000 + f[3]) as f64)
                }
                3 => {
                    let f = self.pdt_fields(idx).unwrap_or([0; 9]);
                    Ok(day_ms(f[0], f[1], f[2])
                        + (f[3] * 3_600_000 + f[4] * 60_000 + f[5] * 1000 + f[6]) as f64)
                }
                4 => {
                    let ns = self.instant_ns(idx).unwrap_or(0);
                    Ok((ns / 1_000_000) as f64)
                }
                // PlainYearMonth (y, m, refDay) / PlainMonthDay (refYear, m, d).
                5 => match self.heap.get(idx) {
                    HeapObj::Temporal { fields, .. } => {
                        Ok(day_ms(fields[0], fields[1], *fields.get(2).unwrap_or(&1)))
                    }
                    _ => Ok(0.0),
                },
                _ => match self.heap.get(idx) {
                    HeapObj::Temporal { fields, .. } => {
                        Ok(day_ms(fields[0], fields[1], *fields.get(2).unwrap_or(&1)))
                    }
                    _ => Ok(0.0),
                },
            };
        }
        let n = self.to_number(v)?;
        if !n.is_finite() || n.abs() > 8.64e15 {
            return Err(Thrown("RangeError: date value is not finite".into()));
        }
        let t = n.trunc();
        Ok(if t == 0.0 { 0.0 } else { t })
    }

    /// Wrap a (type, value[, source]) part list as the Array of plain objects the
    /// *ToParts methods return.
    pub(crate) fn intl_parts_array(&mut self, parts: &[(String, String, &str)]) -> Value {
        self.intl_parts_array_keyed(parts, "source")
    }

    /// As `intl_parts_array`, but the third tuple slot lands under `key` —
    /// `source` for the range formatters, `unit` for RelativeTimeFormat and
    /// DurationFormat. An empty string omits the field entirely (a `literal`
    /// between units belongs to neither).
    pub(crate) fn intl_parts_array_keyed(
        &mut self,
        parts: &[(String, String, &str)],
        key: &str,
    ) -> Value {
        let mut out: Vec<Value> = Vec::with_capacity(parts.len());
        for (ty, val, extra) in parts {
            let mut o = ObjMap::new();
            let t = self.alloc_str(ty.clone());
            o.set("type", t);
            let v = self.alloc_str(val.clone());
            o.set("value", v);
            if !extra.is_empty() {
                let s = self.alloc_str(extra.to_string());
                o.set(key, s);
            }
            out.push(Value::heap(self.heap.alloc(HeapObj::Object(Box::new(o)))));
        }
        Value::heap(self.heap.alloc(HeapObj::Array(out)))
    }

    /// PartitionDateTimeRangePattern: when the two times fall in the same pattern
    /// slot the range collapses to a single formatting with every part "shared";
    /// otherwise the two sides are joined by the en range separator.
    pub(crate) fn dtf_range_parts(
        &self,
        resolved: u32,
        x: f64,
        y: f64,
    ) -> Vec<(String, String, &'static str)> {
        let a = self.dtf_parts(resolved, x);
        let b = self.dtf_parts(resolved, y);
        if a == b {
            return a.into_iter().map(|(t, v)| (t.to_string(), v, "shared")).collect();
        }
        let mut out: Vec<(String, String, &'static str)> =
            a.into_iter().map(|(t, v)| (t.to_string(), v, "startRange")).collect();
        out.push(("literal".to_string(), " \u{2013} ".to_string(), "shared"));
        out.extend(b.into_iter().map(|(t, v)| (t.to_string(), v, "endRange")));
        out
    }

    /// PartitionNumberRangePattern, same shape as the date-time one.
    pub(crate) fn nf_range_parts(
        &mut self,
        resolved: u32,
        x: f64,
        y: f64,
    ) -> Result<Vec<(String, String, &'static str)>, Thrown> {
        let a = self.nf_parts(resolved, x)?;
        let b = self.nf_parts(resolved, y)?;
        if a == b {
            // Both endpoints format identically: the range collapses to the
            // "approximately" pattern (~x), every part shared.
            let mut out: Vec<(String, String, &'static str)> =
                vec![("approximatelySign".to_string(), "~".to_string(), "shared")];
            out.extend(a.into_iter().map(|(t, v)| (t, v, "shared")));
            return Ok(out);
        }
        let mut out: Vec<(String, String, &'static str)> =
            a.into_iter().map(|(t, v)| (t, v, "startRange")).collect();
        out.push(("literal".to_string(), "\u{2013}".to_string(), "shared"));
        out.extend(b.into_iter().map(|(t, v)| (t, v, "endRange")));
        Ok(out)
    }

    /// PartitionNumberPattern: split the formatted number into its typed pieces
    /// (sign, integer runs around the group separators, decimal, fraction, and the
    /// style's affix) rather than returning the whole string as one part.
    pub(crate) fn nf_parts(
        &mut self,
        resolved: u32,
        n: f64,
    ) -> Result<Vec<(String, String)>, Thrown> {
        let formatted = self.intl_number_format_str(resolved, n)?;
        let style = self.display(self.intl_slot(resolved, "style"));
        let currency_prefix = if style == "currency" {
            let cur = self.display(self.intl_slot(resolved, "currency"));
            currency_symbol(&cur)
        } else {
            String::new()
        };
        let mut parts: Vec<(String, String)> = vec![];
        let mut rest = formatted.as_str();
        if let Some(r) = rest.strip_prefix('-') {
            parts.push(("minusSign".into(), "-".into()));
            rest = r;
        } else if let Some(r) = rest.strip_prefix('+') {
            parts.push(("plusSign".into(), "+".into()));
            rest = r;
        }
        if !currency_prefix.is_empty() {
            rest = rest.strip_prefix(currency_prefix.as_str()).unwrap_or(rest);
            parts.push(("currency".into(), currency_prefix.clone()));
        }
        let suffix = if style == "percent" && rest.ends_with('%') {
            rest = &rest[..rest.len() - 1];
            Some(("percentSign".to_string(), "%".to_string()))
        } else {
            None
        };
        // Split off a scientific/engineering exponent before the mantissa is
        // decomposed: it becomes its own exponentSeparator / exponentMinusSign /
        // exponentInteger run at the very end of the part list.
        let exponent = match rest.split_once('E') {
            Some((mantissa, e)) => {
                rest = mantissa;
                Some(e.to_string())
            }
            None => None,
        };
        let (int_str, frac_str) = match rest.split_once('.') {
            Some((i, f)) => (i, Some(f)),
            None => (rest, None),
        };
        if int_str == "NaN" {
            parts.push(("nan".into(), int_str.into()));
        } else if int_str == "\u{221e}" {
            parts.push(("infinity".into(), int_str.into()));
        } else {
            for (i, run) in int_str.split(',').enumerate() {
                if i > 0 {
                    parts.push(("group".into(), ",".into()));
                }
                parts.push(("integer".into(), run.into()));
            }
        }
        if let Some(f) = frac_str {
            parts.push(("decimal".into(), ".".into()));
            parts.push(("fraction".into(), f.into()));
        }
        if let Some(e) = exponent {
            parts.push(("exponentSeparator".into(), "E".into()));
            match e.strip_prefix('-') {
                Some(mag) => {
                    parts.push(("exponentMinusSign".into(), "-".into()));
                    parts.push(("exponentInteger".into(), mag.into()));
                }
                None => parts.push(("exponentInteger".into(), e)),
            }
        }
        if let Some(s) = suffix {
            parts.push(s);
        }
        if style == "unit" {
            let u = self.display(self.intl_slot(resolved, "unit"));
            parts.push(("literal".into(), " ".into()));
            parts.push(("unit".into(), u));
        }
        Ok(parts)
    }

    /// FormatDateTimePattern for the en-US patterns this engine implements, as a
    /// typed part list. `format` is this joined; `formatToParts` is this wrapped.
    /// (The pattern is date-then-time, both fixed: no locale data behind it.)
    pub(crate) fn dtf_parts(&self, resolved: u32, ms: f64) -> Vec<(&'static str, String)> {
        let total_ms = ms as i128;
        let days = total_ms.div_euclid(86_400_000) as i64;
        let (y, mo, d) = epoch_days_to_iso(days);
        let rem_ns = total_ms.rem_euclid(86_400_000) * 1_000_000;
        let t = ns_to_time(rem_ns); // [h, mi, s, ms, us, ns]
        let slot = |k: &str| -> Option<String> {
            match self.heap.get(resolved) {
                HeapObj::Object(m) => m.pos(k).map(|i| self.display(m.vals[i])),
                _ => None,
            }
        };
        let has = |k: &str| slot(k).is_some();
        // Only the components the resolved options actually carry are emitted:
        // `{minute, second}` must format as "02:03", not as a whole date-time.
        // (A `dateStyle`/`timeStyle` selects the full date / full time, since the
        // per-style skeletons are CLDR data this engine does not have.)
        let date_style = has("dateStyle");
        let time_style = has("timeStyle");
        let has_date = has("year") || has("month") || has("day") || has("weekday") || date_style;
        let has_time = has("hour") || has("minute") || has("second") || time_style;
        // A two-digit request pads; a numeric one does not.
        let two = |k: &str, v: i64| -> String {
            if slot(k).as_deref() == Some("2-digit") { format!("{v:02}") } else { v.to_string() }
        };
        let mut out: Vec<(&'static str, String)> = vec![];
        // Neither a date nor a time component resolved (a dayPeriod- or
        // timeZoneName-only request): the date fields carry nothing to print.
        if has_date {
            let mut date: Vec<(&'static str, String)> = vec![];
            if has("month") || date_style {
                date.push(("month", two("month", mo)));
            }
            if has("day") || date_style {
                if !date.is_empty() {
                    date.push(("literal", "/".to_string()));
                }
                date.push(("day", two("day", d)));
            }
            if has("year") || date_style {
                if !date.is_empty() {
                    date.push(("literal", "/".to_string()));
                }
                date.push(("year", y.to_string()));
            }
            out.extend(date);
        }
        if has_time {
            let h24 = t[0];
            let (h12, ap) = if h24 == 0 {
                (12, "AM")
            } else if h24 <= 12 {
                (h24, if h24 == 12 { "PM" } else { "AM" })
            } else {
                (h24 - 12, "PM")
            };
            // hourCycle h23/h24 print the 24-hour clock and no day period.
            let hc = slot("hourCycle").unwrap_or_else(|| "h12".to_string());
            let is12 = hc == "h11" || hc == "h12";
            let hour_val = match hc.as_str() {
                "h11" => h24 % 12,
                "h23" => h24,
                "h24" => {
                    if h24 == 0 { 24 } else { h24 }
                }
                _ => h12,
            };
            let mut time: Vec<(&'static str, String)> = vec![];
            if has("hour") || time_style {
                time.push(("hour", two("hour", hour_val)));
            }
            if has("minute") || time_style {
                if !time.is_empty() {
                    time.push(("literal", ":".to_string()));
                }
                time.push(("minute", format!("{:02}", t[1])));
            }
            if has("second") || time_style {
                if !time.is_empty() {
                    time.push(("literal", ":".to_string()));
                }
                time.push(("second", format!("{:02}", t[2])));
            }
            // fractionalSecondDigits attaches to the second as a `.`-separated
            // `fractionalSecond` part, truncated (never rounded) to its width.
            if let Some(f) = slot("fractionalSecondDigits") {
                if let Ok(n) = f.parse::<usize>() {
                    if n >= 1 && n <= 3 {
                        let ms_str = format!("{:03}", t[3]);
                        time.push(("literal", ".".to_string()));
                        time.push(("fractionalSecond", ms_str[..n].to_string()));
                    }
                }
            }
            // The day period belongs to the HOUR field: `{minute, second}` is
            // "02:03", not "02:03 PM".
            if is12 && (has("hour") || time_style) {
                time.push(("literal", "\u{202f}".to_string()));
                time.push(("dayPeriod", ap.to_string()));
            }
            if !out.is_empty() && !time.is_empty() {
                out.push(("literal", ", ".to_string()));
            }
            out.extend(time);
        }
        out
    }

    /// Intl.DateTimeFormat.prototype.format(date) — UTC, en-US conventions.
    pub(crate) fn dtf_format(&self, resolved: u32, ms: f64) -> String {
        self.dtf_parts(resolved, ms).into_iter().map(|(_, v)| v).collect()
    }

}

/// The resolved digit slots of SetNumberFormatDigitOptions. A `None` pair means
/// that pair is not part of the resolved rounding type and must be left out of
/// resolvedOptions entirely (significant digits and fraction digits are mutually
/// exclusive under the default "auto" roundingPriority).
pub(crate) struct DigitOptions {
    pub min_integer: i64,
    pub min_fraction: Option<i64>,
    pub max_fraction: Option<i64>,
    pub min_significant: Option<i64>,
    pub max_significant: Option<i64>,
    pub rounding_increment: i64,
    pub rounding_mode: String,
    pub rounding_priority: String,
    pub trailing_zero_display: String,
}

/// `useGrouping` is the one option whose resolved value keeps a JS type: `true`
/// and `false` stay booleans, the strategies stay strings.
pub(crate) enum UseGrouping {
    Bool(bool),
    Str(String),
}

/// The calendars and numbering systems this engine actually has data for. The
/// list is the SAME one `Intl.supportedValuesOf` reports, and DateTimeFormat /
/// NumberFormat resolve an option against it — a well-formed but unsupported
/// value (`{calendar: "bangla"}`, `-u-nu-adlm`) falls back to the default rather
/// than being echoed back, which is what the supportedValuesOf round-trip tests
/// and the future-calendar fallback tests require.
pub(crate) const AVAILABLE_CALENDARS: &[&str] = &["gregory", "iso8601"];
pub(crate) const AVAILABLE_NUMBERING_SYSTEMS: &[&str] = &["latn"];

/// Resolve a requested calendar/numberingSystem: ASCII-lowercased (the Unicode
/// extension keys are case-insensitive), accepted only if supported.
fn resolve_available(requested: Option<String>, available: &[&str], default: &str) -> String {
    match requested {
        Some(s) => {
            let lower = s.to_ascii_lowercase();
            if available.contains(&lower.as_str()) { lower } else { default.to_string() }
        }
        None => default.to_string(),
    }
}

/// The characters CLDR's root collation gives *variable* (shifted) weights —
/// whitespace, punctuation and general symbols. `Intl.Collator`'s
/// `ignorePunctuation` drops exactly these before comparing. Approximated from
/// std's Unicode predicates: anything that is neither a letter/number nor a
/// combining mark is variable. (Currency symbols and digits are variable in
/// UCA only above variable-top; without weight tables the distinction is not
/// observable through a code-point comparison.)
pub(crate) fn is_variable_collation_char(c: char) -> bool {
    !c.is_alphanumeric() && !unicode_normalization::char::is_combining_mark(c)
}

/// IsValidDisplayNamesCode + CanonicalCodeForDisplayNames (ECMA-402 12.5.2):
/// validate `Intl.DisplayNames.prototype.of`'s argument against the instance's
/// `type` and return it in canonical case, or `None` for a RangeError. Pure
/// grammar — no display-name data is involved.
pub(crate) fn canonical_display_names_code(ty: &str, code: &str) -> Option<String> {
    match ty {
        "language" => crate::vm::locale_tag::canonicalize_tag(code),
        "region" => {
            let ok = (code.len() == 2 && code.bytes().all(|b| b.is_ascii_alphabetic()))
                || (code.len() == 3 && code.bytes().all(|b| b.is_ascii_digit()));
            ok.then(|| code.to_ascii_uppercase())
        }
        "script" => (code.len() == 4 && code.bytes().all(|b| b.is_ascii_alphabetic())).then(|| {
            let l = code.to_ascii_lowercase();
            format!("{}{}", l[..1].to_ascii_uppercase(), &l[1..])
        }),
        "currency" => (code.len() == 3 && code.bytes().all(|b| b.is_ascii_alphabetic()))
            .then(|| code.to_ascii_uppercase()),
        "calendar" => is_well_formed_type_code(code).then(|| code.to_ascii_lowercase()),
        // dateTimeField takes one of the twelve field names, verbatim.
        _ => [
            "era", "year", "quarter", "month", "weekOfYear", "weekday", "day", "dayPeriod",
            "hour", "minute", "second", "timeZoneName",
        ]
        .contains(&code)
        .then(|| code.to_string()),
    }
}

/// The ISO 15924 script codes written right-to-left. This is a Unicode fact
/// about the scripts themselves (their characters' Bidi_Class), not per-locale
/// CLDR data, so `Intl.Locale.prototype.getTextInfo` can answer it from the
/// script subtag alone.
fn is_rtl_script(script: &str) -> bool {
    matches!(
        script,
        "Adlm" | "Arab" | "Aran" | "Armi" | "Avst" | "Cprt" | "Egyp" | "Elym" | "Gara" | "Hatr"
            | "Hebr" | "Hung" | "Khar" | "Lydi" | "Mand" | "Mani" | "Mend" | "Merc" | "Mero"
            | "Narb" | "Nbat" | "Nkoo" | "Orkh" | "Palm" | "Phli" | "Phlp" | "Phnx" | "Prti"
            | "Rohg" | "Samr" | "Sarb" | "Sogd" | "Sogo" | "Syrc" | "Thaa" | "Todr" | "Yezi"
    )
}

/// A `-u-` keyword value as it is stored: lowercased, with the redundant
/// explicit "true" dropped (`-u-kn-true` canonicalizes to `-u-kn`).
fn canon_ext_value(v: &str) -> String {
    let l = v.to_ascii_lowercase();
    if l == "true" { String::new() } else { l }
}

/// A Unicode locale extension `type` value: 3-8 alphanumerics, optionally
/// repeated (`islamic-civil`). Used to range-check the `calendar` /
/// `numberingSystem` options before any data lookup (ECMA-402 IsWellFormed
/// CalendarCode / IsWellFormedNumberingSystemCode).
pub(crate) fn is_well_formed_type_code(s: &str) -> bool {
    !s.is_empty()
        && s.split('-').all(|p| {
            (3..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric())
        })
}

/// Whether a canonical tag carries a `-u-<key>` keyword at all, with or without
/// a value — `-u-kn` (the canonical form of `-u-kn-true`) has no value subtag,
/// so `unicode_ext_value` alone cannot distinguish it from an absent key.
pub(crate) fn unicode_ext_has_key(tag: &str, key: &str) -> bool {
    match tag.split("-u-").nth(1) {
        Some(ext) => ext.split('-').any(|t| t == key),
        None => false,
    }
}

/// Read a Unicode `-u-` extension keyword out of a canonical language tag
/// (`"en-u-ca-buddhist"`, key "ca" → `Some("buddhist")`). Keys are two
/// alphanumerics; the value is every following subtag until the next key.
pub(crate) fn unicode_ext_value(tag: &str, key: &str) -> Option<String> {
    let ext = tag.split("-u-").nth(1)?;
    let toks: Vec<&str> = ext.split('-').collect();
    let at = toks.iter().position(|t| *t == key)?;
    let mut out: Vec<&str> = vec![];
    for t in &toks[at + 1..] {
        if t.len() == 2 && t.chars().all(|c| c.is_ascii_alphanumeric()) {
            break; // the next keyword key
        }
        out.push(t);
    }
    if out.is_empty() { None } else { Some(out.join("-")) }
}

/// The sanctioned single-unit identifiers of ECMA-402 Table 2. `unit` may name
/// one of these or a `<numerator>-per-<denominator>` pair of them.
const SANCTIONED_UNITS: &[&str] = &[
    "acre", "bit", "byte", "celsius", "centimeter", "day", "degree", "fluid-ounce", "foot",
    "gallon", "gigabit", "gigabyte", "gram", "hectare", "hour", "inch", "kilobit", "kilobyte",
    "kilogram", "kilometer", "liter", "megabit", "megabyte", "meter", "microsecond", "mile",
    "mile-scandinavian", "milliliter", "millimeter", "millisecond", "minute", "month",
    "nanosecond", "ounce", "percent", "petabyte", "pound", "second", "stone", "terabit",
    "terabyte", "week", "yard", "year", "fahrenheit",
];

pub(crate) fn is_well_formed_unit(u: &str) -> bool {
    if SANCTIONED_UNITS.contains(&u) {
        return true;
    }
    match u.split_once("-per-") {
        Some((n, d)) => SANCTIONED_UNITS.contains(&n) && SANCTIONED_UNITS.contains(&d),
        None => false,
    }
}

/// The number of minor-unit digits a currency formats with (ECMA-402
/// CurrencyDigits): 2 unless the ISO 4217 table says otherwise.
pub(crate) fn currency_digits(code: &str) -> i64 {
    match code.to_uppercase().as_str() {
        "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => 3,
        "BIF" | "CLP" | "DJF" | "GNF" | "ISK" | "JPY" | "KMF" | "KRW" | "PYG" | "RWF" | "UGX"
        | "UYI" | "VND" | "VUV" | "XAF" | "XOF" | "XPF" => 0,
        "CLF" => 4,
        _ => 2,
    }
}

/// Accept a time-zone argument for Intl.DateTimeFormat: "UTC" (any case) is
/// canonicalized, and a `±HH:MM`-style offset or an `Area/Location` identifier is
/// taken verbatim. There is no tz database behind this yet — a syntactically
/// valid but unknown IANA id is accepted rather than rejected, which is why the
/// `timeZone`-lookup tests still fail honestly.
pub(crate) fn canonicalize_time_zone(s: &str) -> Option<String> {
    if s.eq_ignore_ascii_case("UTC") {
        return Some("UTC".to_string());
    }
    // "Area/Location", and the single-token legacy ids ("GMT", "EST5EDT") —
    // ECMA-402 keeps those verbatim rather than folding them onto "UTC".
    let ok = s.split('/').all(|p| {
        !p.is_empty()
            && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+'))
    }) && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
    if ok {
        return Some(s.to_string());
    }
    // A numeric UTC offset (`+05:30`) is also a valid identifier.
    let b = s.as_bytes();
    if !b.is_empty() && (b[0] == b'+' || b[0] == b'-') {
        let body = &s[1..];
        let digits: String = body.chars().filter(|c| *c != ':').collect();
        if matches!(digits.len(), 2 | 4 | 6) && digits.chars().all(|c| c.is_ascii_digit()) {
            return Some(s.to_string());
        }
    }
    None
}
