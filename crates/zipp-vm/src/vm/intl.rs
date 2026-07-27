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
            o.set(&k, v);
        }
        Value::heap(self.heap.alloc(HeapObj::Object(Box::new(o))))
    }

    /// CanonicalizeLocaleList(locales) → the requested tags (canonical). Accepts
    /// undefined (→ empty), a string, an Intl.Locale, or an array of those.
    pub(crate) fn canonicalize_locale_list(&mut self, locales: Value) -> Result<Vec<String>, Thrown> {
        let mut out: Vec<String> = vec![];
        let mut push_tag = |out: &mut Vec<String>, s: &str| -> Result<(), Thrown> {
            match canonicalize_locale(s) {
                Some(c) => {
                    if !out.contains(&c) {
                        out.push(c);
                    }
                    Ok(())
                }
                None => Err(Thrown(format!("RangeError: Incorrect locale information: {s}"))),
            }
        };
        if locales == Value::UNDEFINED {
            return Ok(out);
        }
        // A bare string is treated as a one-element list.
        if locales.is_heap() {
            if let HeapObj::Intl { kind: native::INTL_LOCALE, resolved } =
                *self.heap.get(locales.heap_index())
            {
                let bn = self.intl_slot(resolved, "baseName");
                let s = self.display(bn);
                push_tag(&mut out, &s)?;
                return Ok(out);
            }
            if self.heap.is_str_like(locales.heap_index()) {
                let s = self.heap.str_cow(locales.heap_index()).unwrap().into_owned();
                push_tag(&mut out, &s)?;
                return Ok(out);
            }
        } else if !locales.is_heap() {
            // primitive non-string → ToObject would make a wrapper with no indices.
            return Ok(out);
        }
        // Array-like: read length then each element.
        let len_v = self.get_prop(locales, "length")?;
        let len = self.to_number(len_v)?.max(0.0) as usize;
        for i in 0..len {
            let el = self.get_index(locales, Value::int(i as i32))?;
            if el == Value::UNDEFINED {
                continue;
            }
            if el.is_heap() {
                if let HeapObj::Intl { kind: native::INTL_LOCALE, resolved } =
                    *self.heap.get(el.heap_index())
                {
                    let bn = self.intl_slot(resolved, "baseName");
                    let s = self.display(bn);
                    push_tag(&mut out, &s)?;
                    continue;
                }
            }
            if !el.is_heap() || !self.heap.is_str_like(el.heap_index()) {
                if !matches!(self.heap.get(el.heap_index()), HeapObj::Object(_)) {
                    return Err(Thrown(
                        "TypeError: locale list elements must be strings or objects".into(),
                    ));
                }
            }
            let s = self.to_js_string(el)?;
            push_tag(&mut out, &s)?;
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
            INTL_NUMBERFORMAT | INTL_DATETIMEFORMAT | INTL_COLLATOR | INTL_PLURALRULES => {
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
                let usage =
                    self.opt_string(options, "usage", "sort", &["sort", "search"])?;
                let uv = self.alloc_str(usage);
                r.set("usage", uv);
                let sens = self.opt_string(
                    options,
                    "sensitivity",
                    "variant",
                    &["base", "accent", "case", "variant"],
                )?;
                let sv = self.alloc_str(sens);
                r.set("sensitivity", sv);
                r.set("ignorePunctuation", Value::bool(false));
                let col = self.alloc_str("default".to_string());
                r.set("collation", col);
                let nf = if options == Value::UNDEFINED {
                    false
                } else {
                    let v = self.get_prop(options, "numeric")?;
                    v != Value::UNDEFINED && self.truthy(v)
                };
                r.set("numeric", Value::bool(nf));
                let cf = self.opt_string(
                    options,
                    "caseFirst",
                    "false",
                    &["upper", "lower", "false"],
                )?;
                let cfv = self.alloc_str(cf);
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
                let _ = self.opt_string(options, "compactDisplay", "short", &["short", "long"])?;
                let digits = self.read_number_format_digit_options(options, 0, 3, &notation)?;
                let tv = self.alloc_str(t);
                r.set("type", tv);
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
                let st = self.opt_string(options, "style", "long", &["long", "short", "narrow"])?;
                let sv = self.alloc_str(st);
                r.set("style", sv);
                let nm = self.opt_string(options, "numeric", "always", &["always", "auto"])?;
                let nmv = self.alloc_str(nm);
                r.set("numeric", nmv);
                let ns = self.alloc_str("latn".to_string());
                r.set("numberingSystem", ns);
            }
            INTL_SEGMENTER => {
                let g =
                    self.opt_string(options, "granularity", "grapheme", &["grapheme", "word", "sentence"])?;
                let gv = self.alloc_str(g);
                r.set("granularity", gv);
            }
            INTL_DISPLAYNAMES => {
                // type is required for DisplayNames.
                let t = self.opt_string(
                    options,
                    "type",
                    "",
                    &["language", "region", "script", "currency", "calendar", "dateTimeField"],
                )?;
                if t.is_empty() {
                    return Err(Thrown("TypeError: Intl.DisplayNames type option is required".into()));
                }
                let tv = self.alloc_str(t);
                r.set("type", tv);
                let st = self.opt_string(options, "style", "long", &["long", "short", "narrow"])?;
                let sv = self.alloc_str(st);
                r.set("style", sv);
                let fb = self.opt_string(options, "fallback", "code", &["code", "none"])?;
                let fbv = self.alloc_str(fb);
                r.set("fallback", fbv);
            }
            INTL_DURATIONFORMAT => {
                let st =
                    self.opt_string(options, "style", "short", &["long", "short", "narrow", "digital"])?;
                let sv = self.alloc_str(st);
                r.set("style", sv);
                let ns = self.alloc_str("latn".to_string());
                r.set("numberingSystem", ns);
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

    /// `new Intl.Locale(tag, options)` — parse the tag into its subtags.
    pub(crate) fn make_locale(&mut self, tag: Value, options: Value) -> Result<Value, Thrown> {
        let base = if tag.is_heap() {
            if let HeapObj::Intl { kind: native::INTL_LOCALE, resolved } =
                *self.heap.get(tag.heap_index())
            {
                self.display(self.intl_slot(resolved, "baseName"))
            } else if self.heap.is_str_like(tag.heap_index()) {
                self.heap.str_cow(tag.heap_index()).unwrap().into_owned()
            } else {
                return Err(Thrown("TypeError: Locale tag must be a string or Locale".into()));
            }
        } else {
            return Err(Thrown("TypeError: Locale tag must be a string or Locale".into()));
        };
        let canon = canonicalize_locale(&base)
            .ok_or_else(|| Thrown(format!("RangeError: invalid language tag: {base}")))?;
        // Split off any -u- extension; the leading part is the baseName.
        let (base_part, _ext) = match canon.split_once("-u-") {
            Some((b, e)) => (b.to_string(), Some(e.to_string())),
            None => (canon.clone(), None),
        };
        let parts: Vec<&str> = base_part.split('-').collect();
        let language = parts.first().copied().unwrap_or("und").to_string();
        let mut script = String::new();
        let mut region = String::new();
        for p in &parts[1..] {
            if p.len() == 4 && p.chars().all(|c| c.is_ascii_alphabetic()) {
                script = p.to_string();
            } else if (p.len() == 2 && p.chars().all(|c| c.is_ascii_alphabetic()))
                || (p.len() == 3 && p.chars().all(|c| c.is_ascii_digit()))
            {
                region = p.to_string();
            }
        }
        let mut r = ObjMap::new();
        let bn = self.alloc_str(base_part.clone());
        r.set("baseName", bn);
        let lv = self.alloc_str(language);
        r.set("language", lv);
        r.set(
            "script",
            if script.is_empty() { Value::UNDEFINED } else { self.alloc_str(script) },
        );
        r.set(
            "region",
            if region.is_empty() { Value::UNDEFINED } else { self.alloc_str(region) },
        );
        // Options or -u- extension keys can override; read the common ones.
        for (key, uext) in [
            ("calendar", "ca"),
            ("collation", "co"),
            ("hourCycle", "hc"),
            ("caseFirst", "kf"),
            ("numberingSystem", "nu"),
        ] {
            let from_opt = if options != Value::UNDEFINED {
                let v = self.get_prop(options, key)?;
                if v == Value::UNDEFINED { None } else { Some(self.to_js_string(v)?) }
            } else {
                None
            };
            let val = from_opt.or_else(|| {
                _ext.as_ref().and_then(|e| {
                    let toks: Vec<&str> = e.split('-').collect();
                    toks.iter().position(|t| *t == uext).and_then(|i| toks.get(i + 1).map(|s| s.to_string()))
                })
            });
            match val {
                Some(s) => {
                    let sv = self.alloc_str(s);
                    r.set(key, sv);
                }
                None => {
                    r.set(key, Value::UNDEFINED);
                }
            }
        }
        // numeric (kn) → boolean
        let numeric = if options != Value::UNDEFINED {
            let v = self.get_prop(options, "numeric")?;
            if v != Value::UNDEFINED { Some(self.truthy(v)) } else { None }
        } else {
            None
        };
        r.set("numeric", Value::bool(numeric.unwrap_or(false)));
        let resolved = self.heap.alloc(HeapObj::Object(Box::new(r)));
        let idx = self.heap.alloc(HeapObj::Intl { kind: native::INTL_LOCALE, resolved });
        if self.intl_protos[native::INTL_LOCALE as usize] != 0 {
            self.proto_of.insert(idx, Value::heap(self.intl_protos[native::INTL_LOCALE as usize]));
        }
        Ok(Value::heap(idx))
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
        let params = NumFmtParams {
            style: &style,
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
        let mut out: Vec<Value> = Vec::with_capacity(parts.len());
        for (ty, val, src) in parts {
            let mut o = ObjMap::new();
            let t = self.alloc_str(ty.clone());
            o.set("type", t);
            let v = self.alloc_str(val.clone());
            o.set("value", v);
            if !src.is_empty() {
                let s = self.alloc_str(src.to_string());
                o.set("source", s);
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
        let has = |k: &str| matches!(self.heap.get(resolved), HeapObj::Object(m) if m.pos(k).is_some());
        let has_date = has("year") || has("month") || has("day") || has("dateStyle") || has("weekday");
        let has_time = has("hour") || has("minute") || has("second") || has("timeStyle");
        let mut out: Vec<(&'static str, String)> = vec![];
        // Neither a date nor a time component resolved (a dayPeriod- or
        // timeZoneName-only request): still emit the default date pattern.
        if has_date || !has_time {
            out.push(("month", mo.to_string()));
            out.push(("literal", "/".to_string()));
            out.push(("day", d.to_string()));
            out.push(("literal", "/".to_string()));
            out.push(("year", y.to_string()));
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
            if !out.is_empty() {
                out.push(("literal", ", ".to_string()));
            }
            out.push(("hour", h12.to_string()));
            out.push(("literal", ":".to_string()));
            out.push(("minute", format!("{:02}", t[1])));
            out.push(("literal", ":".to_string()));
            out.push(("second", format!("{:02}", t[2])));
            out.push(("literal", "\u{202f}".to_string()));
            out.push(("dayPeriod", ap.to_string()));
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
