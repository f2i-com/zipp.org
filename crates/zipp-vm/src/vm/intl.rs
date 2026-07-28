#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;
use crate::vm::{cldr_en, dtf_pattern};

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

    /// `%Intl%.[[FallbackSymbol]]` — one per realm, described
    /// "IntlLegacyConstructedSymbol" (ECMA-402 "The Intl Object"). Created
    /// lazily; `make_symbol` registers it in `symbol_keys`, which the collector
    /// already roots, so no extra root is needed.
    ///
    /// Per-REALM because intl402/FallbackSymbol/per-realm.js asserts a
    /// `$262.createRealm()` child's fallback symbol is a different symbol from
    /// the main realm's.
    pub(crate) fn intl_fallback_symbol(&mut self) -> Value {
        let realm = self.current_realm_id().unwrap_or(0);
        if let Some(&v) = self.intl_fallback_syms.get(&realm) {
            return v;
        }
        let desc = self.alloc_str("IntlLegacyConstructedSymbol".to_string());
        let v = self.make_symbol(desc);
        self.intl_fallback_syms.insert(realm, v);
        v
    }

    /// ChainDateTimeFormat / ChainNumberFormat (ECMA-402 normative optional):
    /// `Intl.DateTimeFormat.call(obj)` — the constructor invoked WITHOUT `new`
    /// on a `this` that already inherits from it — stores the freshly built
    /// service on `this` under %Intl%.[[FallbackSymbol]] (non-writable,
    /// non-enumerable, non-configurable) and returns `this`. This is the legacy
    /// `Object.create(Intl.DateTimeFormat.prototype)` subclassing idiom; without
    /// it the call simply discarded `this`.
    pub(crate) fn intl_chain_legacy(
        &mut self,
        ctor: Value,
        built: Value,
        this: Value,
    ) -> Result<Value, Thrown> {
        if !self.is_object_value(this) || !self.ordinary_has_instance(ctor, this)? {
            return Ok(built);
        }
        let sym = self.intl_fallback_symbol();
        let key = self.key_of(sym);
        let mut d = ObjMap::new();
        d.set("value", built);
        d.set("writable", Value::FALSE);
        d.set("enumerable", Value::FALSE);
        d.set("configurable", Value::FALSE);
        let desc = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(d))));
        self.object_define_property(this, &key, desc)?;
        Ok(this)
    }

    /// UnwrapDateTimeFormat / UnwrapNumberFormat: a receiver that is not itself
    /// an initialized service but DOES inherit from the constructor yields the
    /// service stashed by `intl_chain_legacy`. The read is a real [[Get]] with
    /// the symbol key, which is what
    /// intl-legacy-constructed-symbol-on-unwrap.js observes through a Proxy.
    pub(crate) fn intl_unwrap_legacy(&mut self, this: Value, kind: u8) -> Result<Value, Thrown> {
        if !self.is_object_value(this) {
            return Ok(this);
        }
        if let HeapObj::Intl { kind: k, .. } = self.heap.get(this.heap_index()) {
            if *k == kind {
                return Ok(this);
            }
        }
        let ctor = self.intl_ctors[kind as usize];
        if ctor == 0 || !self.ordinary_has_instance(Value::heap(ctor), this)? {
            return Ok(this);
        }
        let sym = self.intl_fallback_symbol();
        let key = self.key_of(sym);
        self.get_member(this, &key, this)
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
        // ToNumber, not the infallible `to_number`: an OBJECT option value must
        // run its own valueOf/toString/@@toPrimitive (`{roundingIncrement:
        // {valueOf(){return 5}}}`), and an abrupt from it must propagate. The
        // infallible form cannot call back into JS, so every object option read
        // as NaN and became a bogus RangeError.
        let n = self.to_number_coerce(v)?;
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
        // ToNumber (see `default_number_option`): an object value's valueOf runs.
        let n = self.to_number_coerce(v)?;
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
            // Steps 26-27: a rounding increment is only meaningful against
            // [[RoundingType]] "fractionDigits" — that is, the DEFAULT priority
            // with no significant-digit options in play. An explicit
            // morePrecision/lessPrecision (whose roundingType is that priority,
            // regardless of which options were supplied) is a TypeError too, and
            // it must be a TypeError rather than the min/max RangeError below.
            let rounding_type_is_fraction = out.rounding_priority == "auto" && !has_sd;
            if !rounding_type_is_fraction {
                return Err(Thrown(
                    "TypeError: roundingIncrement requires the fractionDigits rounding type".into(),
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

    /// GetBooleanOrStringNumberFormatOption(options, "useGrouping",
    /// « "min2", "auto", "always" », "always", fallback) — ECMA-402 15.1.2.
    ///
    /// Two steps that a plain "boolean or one-of-three-strings" reading misses:
    /// the boolean `true` resolves to `trueValue` ("always"), NOT to itself, and
    /// the STRINGS "true"/"false" resolve to the fallback instead of throwing
    /// (step 6). Only `false`/falsy stays the boolean `false`, which is why
    /// resolvedOptions still has to keep the two JS types apart.
    fn read_use_grouping(&mut self, options: Value, fallback: &str) -> Result<UseGrouping, Thrown> {
        if options == Value::UNDEFINED {
            return Ok(UseGrouping::Str(fallback.to_string()));
        }
        let v = self.get_prop(options, "useGrouping")?;
        if v == Value::UNDEFINED {
            return Ok(UseGrouping::Str(fallback.to_string()));
        }
        if v == Value::bool(true) {
            return Ok(UseGrouping::Str("always".to_string()));
        }
        if !self.truthy(v) {
            return Ok(UseGrouping::Bool(false));
        }
        let s = self.to_js_string(v)?;
        if s == "true" || s == "false" {
            return Ok(UseGrouping::Str(fallback.to_string()));
        }
        if !["min2", "auto", "always"].contains(&s.as_str()) {
            return Err(Thrown(format!("RangeError: invalid useGrouping value: {s}")));
        }
        Ok(UseGrouping::Str(s))
    }

    /// `new Intl.<service>(locales, options)` → build resolved options + instance.
    pub(crate) fn make_intl(&mut self, kind: u8, locales: Value, options: Value) -> Result<Value, Thrown> {
        self.make_intl_dtf(kind, locales, options, DtfDefaults::Standard)
    }

    /// As `make_intl`, but with CreateDateTimeFormat's `required`/`defaults`
    /// arguments chosen by the caller. `Intl.DateTimeFormat` itself is
    /// (any, date); `Date.prototype.toLocale*String` and every
    /// `Temporal.*.prototype.toLocaleString` each pass their own pair, which is
    /// what decides both which options clear needDefaults and which components
    /// get filled in when none did.
    pub(crate) fn make_intl_dtf(
        &mut self,
        kind: u8,
        locales: Value,
        options: Value,
        dtf_mode: DtfDefaults,
    ) -> Result<Value, Thrown> {
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
        // The `-u-` keywords ResolveLocale kept, filled in by each service for
        // its own [[RelevantExtensionKeys]] and written back over "locale" below.
        let mut ext_used: Vec<(&str, String)> = vec![];
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
                let (nsr, keep) = resolve_ext_key(
                    ns,
                    unicode_ext_value(&locale, "nu"),
                    AVAILABLE_NUMBERING_SYSTEMS,
                    "latn",
                );
                if keep {
                    ext_used.push(("nu", nsr.clone()));
                }
                let nsv = self.alloc_str(nsr);
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
                // CreateDateTimeFormat steps "If required is date and timeStyle
                // is not undefined, throw" / "If required is time and dateStyle
                // is not undefined, throw". This is the ONLY place the required
                // half is observable when the other half's style is asked for:
                // a PlainDate given {dateStyle, timeStyle} still has a non-empty
                // date pattern, so the empty-intersection TypeError below would
                // never fire (`PlainDate/…/datestyle-and-timestyle.js`).
                let (req_date, req_time) = dtf_mode.required();
                if !req_time && time_style.is_some() {
                    return Err(Thrown(
                        "TypeError: timeStyle is not a valid option for this date-only formatter"
                            .into(),
                    ));
                }
                if !req_date && date_style.is_some() {
                    return Err(Thrown(
                        "TypeError: dateStyle is not a valid option for this time-only formatter"
                            .into(),
                    ));
                }
                // needDefaults is cleared only by weekday/year/month/day (the
                // date half) or dayPeriod/hour/minute/second/
                // fractionalSecondDigits (the time half) — and only by the
                // half(ves) `required` names — or by a style. `era` and
                // `timeZoneName` are Table-7 components that clear NEITHER, so
                // `{timeZoneName: "long"}` and `{era: "short"}` both still
                // resolve to the defaults, which is what every
                // `…-formatting-timezonename.js` test reads back out of
                // formatToParts.
                let clears = vals.iter().any(|(n, _)| match *n {
                    "weekday" | "year" | "month" | "day" => req_date,
                    "dayPeriod" | "hour" | "minute" | "second" => req_time,
                    _ => false,
                }) || (frac_digits.is_some() && req_time);
                if !clears && date_style.is_none() && time_style.is_none() {
                    let (def_date, def_time, def_zone) = dtf_mode.defaults();
                    if def_date {
                        for (name, v) in
                            [("year", "numeric"), ("month", "numeric"), ("day", "numeric")]
                        {
                            vals.push((name, v.to_string()));
                        }
                    }
                    if def_time {
                        for (name, v) in
                            [("hour", "numeric"), ("minute", "numeric"), ("second", "numeric")]
                        {
                            vals.push((name, v.to_string()));
                        }
                    }
                    // ZonedDateTime's defaults group is the only one that names
                    // a zone: `zdt.toLocaleString()` must print the zone name
                    // (`default-includes-time-and-time-zone-name.js`) where the
                    // otherwise identical Instant must not.
                    if def_zone && !vals.iter().any(|(n, _)| *n == "timeZoneName") {
                        vals.push(("timeZoneName", "short".to_string()));
                    }
                    if dtf_mode == DtfDefaults::Standard {
                        // Remembered because each Temporal type re-runs
                        // ToDateTimeOptions with ITS group as the defaults: a
                        // year/month/day that the OPTIONS asked for pins a
                        // PlainTime to a date-only pattern (TypeError), one that
                        // ToDateTimeOptions filled in does not (the PlainTime
                        // pattern becomes hour/minute/second instead). The
                        // toLocaleString modes already filled in their own
                        // type's group, so they must NOT be re-defaulted.
                        r.set("@@dtfDefaulted", Value::bool(true));
                    }
                    // resolvedOptions reports the components in Table-7 order, and
                    // `vals` was built by walking `comps` — restore that order
                    // after the appends (era must precede year, timeZoneName
                    // follow day).
                    let order = |n: &str| {
                        comps.iter().position(|(c, _)| *c == n).unwrap_or(usize::MAX)
                    };
                    vals.sort_by_key(|(n, _)| order(n));
                }
                // The -u-ca / -u-nu extension keywords lose to an explicit option.
                let ext = |k: &str| unicode_ext_value(&locale, k);
                let (calr, keep_ca) =
                    resolve_ext_key(cal_opt, ext("ca"), AVAILABLE_CALENDARS, "gregory");
                if keep_ca {
                    ext_used.push(("ca", calr.clone()));
                }
                let calv = self.alloc_str(calr);
                r.set("calendar", calv);
                let (nsr, keep_nu) =
                    resolve_ext_key(ns_opt, ext("nu"), AVAILABLE_NUMBERING_SYSTEMS, "latn");
                if keep_nu {
                    ext_used.push(("nu", nsr.clone()));
                }
                let nsv = self.alloc_str(nsr);
                r.set("numberingSystem", nsv);
                let tzv = self.alloc_str(tz);
                r.set("timeZone", tzv);
                // hourCycle/hour12 are reported only when the resolved pattern has
                // an hour field — an explicit `hour` component or any timeStyle.
                let has_hour = vals.iter().any(|(n, _)| *n == "hour") || time_style.is_some();
                if has_hour {
                    // An explicit `hour12` overrides both the `hourCycle` option
                    // and `-u-hc-`, and (ECMA-402) drops the keyword from the
                    // resolved locale; otherwise this is an ordinary
                    // option-then-extension resolution.
                    let hc = match (hour12, hour_cycle_opt.clone()) {
                        // hour12:false is h23; hour12:true keeps the locale's
                        // 12-hour cycle (h12 for the en-style default).
                        (Some(false), _) => "h23".to_string(),
                        (Some(true), _) => "h12".to_string(),
                        (None, opt) => {
                            let (v, keep) = resolve_ext_key(
                                opt,
                                ext("hc"),
                                &["h11", "h12", "h23", "h24"],
                                "h12",
                            );
                            if keep {
                                ext_used.push(("hc", v.clone()));
                            }
                            v
                        }
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
                // "standard"/"search" are never valid as a `-u-co-` value, and
                // only the root collation exists here, so every request resolves
                // to "default" and the keyword is never reflected.
                let _ = collation.or_else(|| unicode_ext_value(&locale, "co"));
                let col = self.alloc_str("default".to_string());
                r.set("collation", col);
                // `-u-kn` with no value IS the canonical spelling of `-u-kn-true`,
                // so key presence (not a "true" value) is what turns it on.
                let ext_kn = unicode_ext_has_key(&locale, "kn").then(|| {
                    match unicode_ext_value(&locale, "kn").as_deref() {
                        Some("false") => "false".to_string(),
                        _ => "true".to_string(),
                    }
                });
                let (kn_s, keep_kn) = resolve_ext_key(
                    numeric.map(|b| b.to_string()),
                    ext_kn,
                    &["true", "false"],
                    "false",
                );
                if keep_kn {
                    // The canonical spelling of `-u-kn-true` is the bare key.
                    ext_used.push(("kn", if kn_s == "true" { String::new() } else { kn_s.clone() }));
                }
                r.set("numeric", Value::bool(kn_s == "true"));
                let (kf, keep_kf) = resolve_ext_key(
                    case_first,
                    unicode_ext_value(&locale, "kf"),
                    &["upper", "lower", "false"],
                    "false",
                );
                if keep_kf {
                    ext_used.push(("kf", kf.clone()));
                }
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
                let (nsr, keep) = resolve_ext_key(
                    ns_opt,
                    unicode_ext_value(&locale, "nu"),
                    AVAILABLE_NUMBERING_SYSTEMS,
                    "latn",
                );
                if keep {
                    ext_used.push(("nu", nsr.clone()));
                }
                let ns = self.alloc_str(nsr);
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
                let (nsr, keep) = resolve_ext_key(
                    ns_opt,
                    unicode_ext_value(&locale, "nu"),
                    AVAILABLE_NUMBERING_SYSTEMS,
                    "latn",
                );
                if keep {
                    ext_used.push(("nu", nsr.clone()));
                }
                let nsv = self.alloc_str(nsr);
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
        // ResolveLocale step: [[locale]] is the base tag plus the kept keywords.
        // A service with no relevant extension keys (ListFormat, Segmenter,
        // DisplayNames) therefore reports the tag with NO `-u-` at all.
        let tag = resolved_locale_tag(&locale, &ext_used);
        let lv = self.alloc_str(tag);
        r.set("locale", lv);
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
        // ApplyOptionsToTag canonicalizes the tag BEFORE the options are applied
        // and again after (steps 9 and 13), and the two passes are not
        // interchangeable: `new Intl.Locale("und-Armn-SU", {language: "ru"})` is
        // "ru-Armn-AM" because SU resolves through the likely region of
        // *und-Armn*, not of ru (constructor-apply-options-canonicalizes-twice).
        crate::vm::cldr_alias::canonicalize(&mut t);
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
        // ApplyOptionsToTag step 13 — a region supplied as an option is itself
        // subject to the registries (`{region: "554"}` on "en" is "en-NZ").
        crate::vm::cldr_alias::canonicalize(&mut t);
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
        // The keyword types the options just wrote are canonicalized too, so
        // `new Intl.Locale("en", {calendar: "islamicc"}).calendar` reports
        // "islamic-civil" exactly as the `-u-ca-islamicc` spelling does
        // (constructor-options-canonicalized.js). The language-id pass is
        // idempotent by now, so re-running the whole thing is safe.
        crate::vm::cldr_alias::canonicalize(&mut t);
        Ok(self.alloc_locale(&t))
    }

    /// CompareStrings for a resolved `Intl.Collator`.
    ///
    /// There is no DUCET/CLDR collation here — this is an NFC code-point
    /// comparison. The two option-driven transforms that do NOT need collation
    /// weights are applied: `ignorePunctuation` drops the characters CLDR treats
    /// as variable (whitespace, punctuation and symbols), and
    /// `sensitivity: "base"/"accent"` case-folds.
    ///
    /// Shared with `String.prototype.localeCompare`, which ECMA-402 specifies as
    /// `Intl.Collator(locales, options).compare(this, that)` — so the two agree
    /// by construction rather than by two copies of the same approximation
    /// (`localeCompare/returns-same-results-as-Collator.js`).
    pub(crate) fn collator_compare(&self, resolved: u32, a: &str, b: &str) -> f64 {
        use unicode_normalization::UnicodeNormalization;
        let ignore_punct = self.intl_slot(resolved, "ignorePunctuation") == Value::bool(true);
        let sens = self.display(self.intl_slot(resolved, "sensitivity"));
        let prep = |s: &str| -> String {
            let s: String = s.nfc().collect();
            let s: String = if ignore_punct {
                s.chars().filter(|c| !is_variable_collation_char(*c)).collect()
            } else {
                s
            };
            if sens == "base" || sens == "accent" { s.to_lowercase() } else { s }
        };
        let (a, b) = (prep(a), prep(b));
        if a < b {
            -1.0
        } else if a > b {
            1.0
        } else {
            0.0
        }
    }

    /// CreateSegmentsObject / CreateSegmentIterator — the two branded objects
    /// `Intl.Segmenter.prototype.segment` hands out. Both hold the same three
    /// slots ([[SegmentsString]] / [[IteratedString]], the granularity, and the
    /// iterator's cursor), so one constructor covers both; `kind` is what
    /// `containing`/`next` brand-check against.
    ///
    /// `input` is ToString'd here, which is `segment`'s step 3 — a Symbol
    /// argument is a TypeError, and an object's `toString` runs exactly once
    /// (segment-tostring.js).
    pub(crate) fn make_segments(
        &mut self,
        kind: u8,
        input: Value,
        granularity: &str,
        index: usize,
    ) -> Result<Value, Thrown> {
        // A string argument is kept as-is (flattened) rather than round-tripped
        // through `to_js_string`, whose lossy view would turn a lone surrogate
        // into U+FFFD; ToString on a String is the identity anyway.
        let sv = if input.is_heap() && self.heap.is_str_like(input.heap_index()) {
            self.heap.flatten(input.heap_index());
            input
        } else {
            let s = self.to_js_string(input)?;
            self.alloc_str(s)
        };
        let mut r = ObjMap::new();
        r.set("input", sv);
        let g = self.alloc_str(granularity.to_string());
        r.set("granularity", g);
        r.set("index", Value::num(index as f64));
        let resolved = self.heap.alloc(HeapObj::Object(Box::new(r)));
        let idx = self.heap.alloc(HeapObj::Intl { kind, resolved });
        if self.intl_protos[kind as usize] != 0 {
            self.proto_of.insert(idx, Value::heap(self.intl_protos[kind as usize]));
        }
        Ok(Value::heap(idx))
    }

    /// CreateSegmentDataObject: an ordinary object whose own property NAMES are
    /// exactly `segment`, `index`, `input` — plus `isWordLike` at
    /// `granularity: "word"` and at no other granularity, which
    /// `segment-{grapheme,word,sentence}-iterable.js` each assert directly.
    pub(crate) fn segment_data_object(
        &mut self,
        input: Value,
        granularity: &str,
        start: usize,
        end: usize,
    ) -> Value {
        let piece = match self.heap.get(input.heap_index()) {
            HeapObj::Str(js) => js.slice_units(start, end),
            _ => return Value::UNDEFINED,
        };
        let seg = Value::heap(self.heap.alloc_js(piece));
        let mut o = ObjMap::new();
        o.set("segment", seg);
        o.set("index", Value::num(start as f64));
        o.set("input", input);
        if granularity == "word" {
            let text = self.display(input);
            let wl = crate::vm::segmenter::is_word_like(&text, start);
            o.set("isWordLike", Value::bool(wl));
        }
        Value::heap(self.heap.alloc(HeapObj::Object(Box::new(o))))
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
        // Number Format Functions step 4 is ? ToNumber(value) — the FULL one, so
        // an object argument's @@toPrimitive/valueOf runs exactly once and its
        // exception propagates (`format({[Symbol.toPrimitive](){…}})`).
        let n = self.to_number_coerce(value)?;
        // FormatNumeric is FormatNumericToParts concatenated, so derive it from
        // the SAME PartitionNumberPattern: `format` used to re-derive the string
        // on its own and dropped the `unit` style's affix that `formatToParts`
        // emits — `new Intl.NumberFormat("en",{style:"unit",unit:"day"})`
        // formatted 1 as "1" while its parts said "1 day". Intl.DurationFormat
        // builds its output from those parts and test262 grades it against
        // `format`, so the two disagreeing is directly observable.
        // (nf_parts applies the numbering system to the digit runs — and only
        // to them; the separators around them are locale data this engine lacks.)
        let parts = self.nf_parts(resolved, n)?;
        let s: String = parts.into_iter().map(|(_, v)| v).collect();
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
        let group_min2 = grouping && self.display(ug) == "min2";
        let notation = self.display(self.intl_slot(resolved, "notation"));
        let compact_display = self.display(self.intl_slot(resolved, "compactDisplay"));
        let params = NumFmtParams {
            style: &style,
            notation: &notation,
            compact_display: &compact_display,
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
            group_min2,
        };
        let s = format_number_intl(n, &params);
        Ok(if style == "currency" {
            let cur = self.display(self.intl_slot(resolved, "currency"));
            let sym = currency_symbol(&cur);
            // `currencySign: "accounting"` swaps CLDR's *negative subpattern* in
            // for the minus sign — for `en` that is `(¤#,##0.00)`, so -987 USD
            // reads "($987.00)". Which values take it is decided upstream: the
            // parentheses appear exactly when signDisplay left a minus behind
            // (so `signDisplay:"never"` and `exceptZero`'s -0 keep "$0.00").
            let accounting = self.display(self.intl_slot(resolved, "currencySign")) == "accounting";
            match s.strip_prefix('-') {
                Some(rest) if accounting => {
                    let (pre, post) = accounting_affixes();
                    format!("{pre}{sym}{rest}{post}")
                }
                // The sign leads the currency symbol ("-$5.00", "+$5.00"), so
                // splice the symbol in after it rather than prefixing the whole
                // string.
                Some(rest) => format!("-{sym}{rest}"),
                None => match s.strip_prefix('+') {
                    Some(rest) => format!("+{sym}{rest}"),
                    None => format!("{sym}{s}"),
                },
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

    /// One bit per DateTimeFormat component (ECMA-402 Table 7 order).
    ///
    /// ECMA-402's Temporal integration gives one DateTimeFormat a *separate*
    /// resolved pattern per Temporal type ([[TemporalPlainDateFormat]] and
    /// friends): each is ToDateTimeOptions(originalOptions, ANY, <that type's
    /// group>) intersected with the fields that type actually has, and an empty
    /// result makes HandleDateTimeValue throw a TypeError. Masks express both
    /// halves — which parts print, and whether anything prints at all.
    pub(crate) const F_ERA: u16 = 1;
    pub(crate) const F_YEAR: u16 = 2;
    pub(crate) const F_MONTH: u16 = 4;
    pub(crate) const F_DAY: u16 = 8;
    pub(crate) const F_WEEKDAY: u16 = 16;
    pub(crate) const F_DAYPERIOD: u16 = 32;
    pub(crate) const F_HOUR: u16 = 64;
    pub(crate) const F_MINUTE: u16 = 128;
    pub(crate) const F_SECOND: u16 = 256;
    pub(crate) const F_FRAC: u16 = 512;
    pub(crate) const F_ZONE: u16 = 1024;
    const F_DATE: u16 = Self::F_ERA | Self::F_YEAR | Self::F_MONTH | Self::F_DAY | Self::F_WEEKDAY;
    const F_TIME: u16 =
        Self::F_DAYPERIOD | Self::F_HOUR | Self::F_MINUTE | Self::F_SECOND | Self::F_FRAC;

    /// (allowed, defaults) for a `dt_arg_kind` — the fields a value of that type
    /// can contribute, and the group ToDateTimeOptions fills in when the options
    /// named no component at all.
    fn temporal_fields(kind: u8) -> (u16, u16) {
        const YMD: u16 = Vm::F_YEAR | Vm::F_MONTH | Vm::F_DAY;
        const HMS: u16 = Vm::F_HOUR | Vm::F_MINUTE | Vm::F_SECOND;
        match kind {
            1 => (Self::F_DATE, YMD),                     // PlainDate
            2 => (Self::F_TIME, HMS),                     // PlainTime
            3 => (Self::F_DATE | Self::F_TIME, YMD | HMS), // PlainDateTime (no zone)
            // ZonedDateTime — reachable only from its own toLocaleString
            // (Intl.DateTimeFormat rejects the type outright), where the zone
            // name is part of the defaults group.
            7 => (Self::F_DATE | Self::F_TIME | Self::F_ZONE, YMD | HMS | Self::F_ZONE),
            5 => (Self::F_ERA | Self::F_YEAR | Self::F_MONTH, Self::F_YEAR | Self::F_MONTH),
            6 => (Self::F_MONTH | Self::F_DAY, Self::F_MONTH | Self::F_DAY),
            _ => (Self::F_DATE | Self::F_TIME | Self::F_ZONE, YMD | HMS), // Instant
        }
    }

    /// The components `resolved` names. `dateStyle`/`timeStyle` stand for whole
    /// groups; `full`/`long` time styles also carry the zone name, which is why
    /// a PlainDateTime formatted with `timeStyle: "long"` must print less than an
    /// Instant does.
    pub(crate) fn dtf_requested_fields(&self, resolved: u32) -> u16 {
        let has = |k: &str| self.intl_slot(resolved, k) != Value::UNDEFINED;
        let mut m = 0u16;
        for (name, bit) in [
            ("era", Self::F_ERA),
            ("year", Self::F_YEAR),
            ("month", Self::F_MONTH),
            ("day", Self::F_DAY),
            ("weekday", Self::F_WEEKDAY),
            ("dayPeriod", Self::F_DAYPERIOD),
            ("hour", Self::F_HOUR),
            ("minute", Self::F_MINUTE),
            ("second", Self::F_SECOND),
            ("fractionalSecondDigits", Self::F_FRAC),
            ("timeZoneName", Self::F_ZONE),
        ] {
            if has(name) {
                m |= bit;
            }
        }
        if has("dateStyle") {
            m |= Self::F_YEAR | Self::F_MONTH | Self::F_DAY;
            // CLDR's `en` full date pattern is `EEEE, MMMM d, y` — the only one
            // of the four that carries a weekday, so `dateStyle: "full"` is the
            // only style whose component set includes one ("Saturday, May 1,
            // 1886" against "May 1, 1886").
            if self.display(self.intl_slot(resolved, "dateStyle")) == "full" {
                m |= Self::F_WEEKDAY;
            }
        }
        if has("timeStyle") {
            m |= Self::F_HOUR | Self::F_MINUTE | Self::F_SECOND;
            let ts = self.display(self.intl_slot(resolved, "timeStyle"));
            if ts == "full" || ts == "long" {
                m |= Self::F_ZONE;
            }
        }
        m
    }

    /// HandleDateTimeValue's field resolution for one argument. Returns the
    /// components to print and whether the value is an ABSOLUTE time (a legacy
    /// Date/number or a Temporal.Instant, which renders in the formatter's time
    /// zone; a plain Temporal value renders its own wall clock, because the
    /// spec's local -> epoch -> local round trip through the same zone cancels).
    ///
    /// A `Temporal.*` value whose fields do not intersect the pattern at all is
    /// a TypeError (`temporal-objects-not-overlapping-options.js`): the per-type
    /// format record would be null.
    pub(crate) fn dtf_fields_for(
        &mut self,
        resolved: u32,
        v: Value,
        name: &str,
    ) -> Result<(u16, bool), Thrown> {
        let requested = self.dtf_requested_fields(resolved);
        let Some(kind) = self.dt_arg_kind(v) else { return Ok((requested, true)) };
        if matches!(kind, 0 | 7) {
            return Ok((requested, true)); // Duration/ZonedDateTime: dtf_time_value rejects
        }
        self.dtf_fields_for_kind(resolved, kind, name)
    }

    /// `dtf_fields_for` with the Temporal kind supplied directly, so
    /// `Temporal.ZonedDateTime.prototype.toLocaleString` — the one caller for
    /// which a ZonedDateTime is a legal argument — can reach the same resolution.
    pub(crate) fn dtf_fields_for_kind(
        &mut self,
        resolved: u32,
        kind: u8,
        name: &str,
    ) -> Result<(u16, bool), Thrown> {
        let requested = self.dtf_requested_fields(resolved);
        let (allowed, defaults) = Self::temporal_fields(kind);
        // ToDateTimeOptions' needDefaults: only a Table-7 date/time COMPONENT
        // clears it — `era` and `timeZoneName` do not (which is why
        // `{timeZoneName: "long"}` still resolves to year/month/day), but a
        // dateStyle/timeStyle does.
        let clears = Self::F_YEAR
            | Self::F_MONTH
            | Self::F_DAY
            | Self::F_WEEKDAY
            | Self::F_DAYPERIOD
            | Self::F_HOUR
            | Self::F_MINUTE
            | Self::F_SECOND
            | Self::F_FRAC;
        let styled = self.intl_slot(resolved, "dateStyle") != Value::UNDEFINED
            || self.intl_slot(resolved, "timeStyle") != Value::UNDEFINED;
        // `@@dtfDefaulted` marks a year/month/day that ToDateTimeOptions supplied
        // rather than the caller — those must not clear needDefaults here.
        let defaulted = self.intl_slot(resolved, "@@dtfDefaulted") == Value::bool(true);
        let need_defaults = defaulted || (requested & clears == 0 && !styled);
        // ToDateTimeOptions ADDS the defaults group to the options; it does not
        // replace them. `{era: "narrow"}` therefore resolves to era + the date
        // defaults, so an Instant formatted by that formatter still prints its
        // era — which is what makes it agree with `Date.prototype.toLocaleString`
        // under the same options (`format/temporal-objects-format-with-era.js`).
        let effective = if need_defaults { requested | defaults } else { requested } & allowed;
        if effective == 0 {
            return Err(Thrown(format!(
                "TypeError: {name} options do not include any field this Temporal value has"
            )));
        }
        // Instant and ZonedDateTime are absolute times: they render THROUGH the
        // formatter's zone. Every plain type carries its own wall clock instead.
        Ok((effective, matches!(kind, 4 | 7)))
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
        // ToNumber then TimeClip. The full ToNumber: a plain object argument
        // ToPrimitive's first, so a throwing `valueOf` wins over the RangeError
        // that a NaN would otherwise produce (`argument-tonumber-throws`).
        let n = self.to_number_coerce(v)?;
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
        fields: u16,
        absolute: bool,
    ) -> Vec<(String, String, &'static str)> {
        let a = self.dtf_parts(resolved, x, fields, absolute);
        let b = self.dtf_parts(resolved, y, fields, absolute);
        if a == b {
            return a.into_iter().map(|(t, v)| (t.to_string(), v, "shared")).collect();
        }
        // PartitionDateTimeRangePattern: CLDR's `intervalFormats` give a pattern
        // per (skeleton, greatest differing field) that names each field twice,
        // so the parts the endpoints SHARE are printed once — "Jan 3 – 5, 2019",
        // and "8/4/2021, 12:30:45 AM – 11:30:45 PM" when only the time moves.
        if let Some(parts) = self.dtf_interval_parts(resolved, x, y, fields, absolute) {
            return parts;
        }
        // `intervalFormatFallback` — format both endpoints whole and join.
        let (pre, post) =
            cldr_en::INTERVAL_FALLBACK.split_once("{0}").unwrap_or(("", cldr_en::INTERVAL_FALLBACK));
        let sep = post.split_once("{1}").map(|(s, _)| s).unwrap_or(" \u{2013} ");
        let mut out: Vec<(String, String, &'static str)> = vec![];
        if !pre.is_empty() {
            out.push(("literal".to_string(), pre.to_string(), "shared"));
        }
        out.extend(a.into_iter().map(|(t, v)| (t.to_string(), v, "startRange")));
        out.push(("literal".to_string(), sep.to_string(), "shared"));
        out.extend(b.into_iter().map(|(t, v)| (t.to_string(), v, "endRange")));
        out
    }

    /// The `intervalFormats` rendering of a range, or None when CLDR has no
    /// pattern for this skeleton and difference (the caller then falls back).
    fn dtf_interval_parts(
        &self,
        resolved: u32,
        x: f64,
        y: f64,
        fields: u16,
        absolute: bool,
    ) -> Option<Vec<(String, String, &'static str)>> {
        let (d_items, t_items, glue) = self.dtf_pattern_halves(resolved, fields);
        let (_, hour12) = self.dtf_request(resolved, fields);
        // Which half moved decides the shape: a date difference ranges the whole
        // value, a time-only difference keeps the date as a shared prefix.
        let d_diff = self.dtf_greatest_diff(resolved, &d_items, x, y, absolute);
        let t_diff = self.dtf_greatest_diff(resolved, &t_items, x, y, absolute);
        if d_diff.is_some() && !t_items.is_empty() {
            // Both halves in play: CLDR has no combined interval skeleton, and
            // splicing two independent ranges would print the date twice inside
            // one range. The fallback pattern is the specified answer.
            return None;
        }
        let (half, greatest) = match (d_diff, t_diff) {
            (Some(g), _) => (&d_items, g),
            (None, Some(g)) => (&t_items, g),
            (None, None) => return None,
        };
        let pattern = dtf_pattern::interval_pattern(half, hour12, greatest)?;
        let items = dtf_pattern::parse_pattern(&pattern);
        let (sep, first, last) = dtf_pattern::interval_layout(&items)?;
        // Items outside the two ranged runs are common to both endpoints.
        let mut ranged: Vec<(String, String, &'static str)> = vec![];
        let mut push = |vm: &Self, slice: &[dtf_pattern::Item], at: f64, src: &'static str| {
            ranged.extend(
                vm.dtf_render(resolved, at, absolute, slice)
                    .into_iter()
                    .map(|(t, v)| (t.to_string(), v, src)),
            );
        };
        push(self, &items[..first], x, "shared");
        push(self, &items[first..sep], x, "startRange");
        push(self, &items[sep..sep + 1], x, "shared");
        push(self, &items[sep + 1..last + 1], y, "endRange");
        push(self, &items[last + 1..], y, "shared");
        if d_diff.is_some() || d_items.is_empty() {
            return Some(ranged);
        }
        // Time-only difference with a date half: the date is shared, and so is
        // the glue between them.
        let shared: Vec<(String, String, &'static str)> = self
            .dtf_render(resolved, x, absolute, &d_items)
            .into_iter()
            .map(|(t, v)| (t.to_string(), v, "shared"))
            .collect();
        Some(dtf_pattern::splice_glue_parts(cldr_en::DATETIME_GLUE_AT[glue], &shared, &ranged))
    }

    /// The most significant field in `items` whose rendering differs between the
    /// two instants — the `intervalFormats` key.
    fn dtf_greatest_diff(
        &self,
        resolved: u32,
        items: &[dtf_pattern::Item],
        x: f64,
        y: f64,
        absolute: bool,
    ) -> Option<char> {
        if items.is_empty() {
            return None;
        }
        let a = self.dtf_render(resolved, x, absolute, items);
        let b = self.dtf_render(resolved, y, absolute, items);
        // `dtf_render` emits exactly one part per item, so the three align.
        let mut best: Option<(u8, char)> = None;
        for (i, item) in items.iter().enumerate() {
            let dtf_pattern::Item::Field(c, _) = item else { continue };
            if a.get(i).map(|p| &p.1) == b.get(i).map(|p| &p.1) {
                continue;
            }
            let Some(k) = dtf_pattern::class_of_pub(*c) else { continue };
            if best.is_none_or(|(bk, _)| k < bk) {
                best = Some((k, *c));
            }
        }
        best.map(|(_, c)| c)
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
            let mut out: Vec<(String, String, &'static str)> = vec![(
                "approximatelySign".to_string(),
                cldr_en::SYM_APPROX_SIGN.to_string(),
                "shared",
            )];
            out.extend(a.into_iter().map(|(t, v)| (t, v, "shared")));
            return Ok(out);
        }
        // CLDR `miscPatterns.range` — `en`'s is the tight "{0}–{1}".
        let sep = cldr_en::PATTERN_RANGE
            .split_once("{0}")
            .and_then(|(_, r)| r.split_once("{1}"))
            .map(|(s, _)| s)
            .unwrap_or("\u{2013}");
        let mut out: Vec<(String, String, &'static str)> =
            a.into_iter().map(|(t, v)| (t, v, "startRange")).collect();
        out.push(("literal".to_string(), sep.to_string(), "shared"));
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
        // The accounting subpattern's affixes are `literal` parts and REPLACE the
        // minusSign part, so they are peeled before the sign check below
        // (`formatToParts/signDisplay-currency-en-US.js` expects
        // [literal "(", currency "$", …, literal ")"] with no minusSign).
        let mut acct_close = "";
        if style == "currency"
            && self.display(self.intl_slot(resolved, "currencySign")) == "accounting"
        {
            let (pre, post) = accounting_affixes();
            if !pre.is_empty() && rest.starts_with(pre) && rest.ends_with(post) {
                rest = &rest[pre.len()..rest.len() - post.len()];
                parts.push(("literal".into(), pre.to_string()));
                acct_close = post;
            }
        }
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
        // The compact affix ("K", " million") is a `compact` part, with any
        // space before it a `literal` — the same split the unit affix gets.
        let mut compact: Option<(String, String)> = None;
        if self.display(self.intl_slot(resolved, "notation")) == "compact" {
            let disp = self.display(self.intl_slot(resolved, "compactDisplay"));
            if let Some(aff) = compact_affix_of(rest, &disp) {
                rest = &rest[..rest.len() - aff.len()];
                let core = aff.trim_start();
                compact = Some((aff[..aff.len() - core.len()].to_string(), core.to_string()));
            }
        }
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
        if let Some((space, core)) = compact {
            if !space.is_empty() {
                parts.push(("literal".into(), space));
            }
            parts.push(("compact".into(), core));
        }
        if let Some(s) = suffix {
            parts.push(s);
        }
        if !acct_close.is_empty() {
            parts.push(("literal".into(), acct_close.to_string()));
        }
        if style == "unit" {
            let u = self.display(self.intl_slot(resolved, "unit"));
            let disp = self.display(self.intl_slot(resolved, "unitDisplay"));
            // `en`'s cardinal rule is `i = 1 and v = 0`: exactly one integer run
            // reading "1" and no fraction. The operands come from the FORMATTED
            // number, so `{minimumFractionDigits: 1}` on 1 selects "other".
            let ints: Vec<&String> =
                parts.iter().filter(|(t, _)| t == "integer").map(|(_, v)| v).collect();
            let one = ints.len() == 1 && ints[0] == "1"
                && !parts.iter().any(|(t, _)| t == "fraction");
            let pattern = cldr_unit_pattern(&u, &disp, one);
            let (prefix, suffix) = match pattern.as_deref().and_then(|p| p.split_once("{0}")) {
                Some((p, s)) => (p.to_string(), s.to_string()),
                // No CLDR row: echo the identifier, as before this table existed.
                None => (String::new(), format!(" {u}")),
            };
            // Inside an affix the spacing is a `literal` part and the name itself
            // a `unit` part — "{0} km/h" is [literal " ", unit "km/h"] while
            // "{0}%" is just [unit "%"] (`formatToParts/percent-en-US.js`).
            let is_sp = |c: char| c.is_whitespace() || c == '\u{a0}' || c == '\u{202f}';
            if !prefix.is_empty() {
                let core = prefix.trim_end_matches(is_sp);
                let mut head: Vec<(String, String)> = vec![];
                if !core.is_empty() {
                    head.push(("unit".into(), core.to_string()));
                }
                if core.len() < prefix.len() {
                    head.push(("literal".into(), prefix[core.len()..].to_string()));
                }
                head.extend(parts);
                parts = head;
            }
            if !suffix.is_empty() {
                let core = suffix.trim_start_matches(is_sp);
                let lead = &suffix[..suffix.len() - core.len()];
                if !lead.is_empty() {
                    parts.push(("literal".into(), lead.to_string()));
                }
                if !core.is_empty() {
                    parts.push(("unit".into(), core.to_string()));
                }
            }
        }
        // The split above works on the ASCII form (it looks for '-', ',', '.',
        // 'E'); the numbering system applies to the digit runs afterwards.
        let ns = self.display(self.intl_slot(resolved, "numberingSystem"));
        if ns != "latn" {
            for (ty, v) in parts.iter_mut() {
                if matches!(ty.as_str(), "integer" | "fraction" | "exponentInteger") {
                    *v = translate_digits(v, &ns);
                }
            }
        }
        Ok(parts)
    }

    /// PartitionDurationFormatPattern (ECMA-402 §1.1.14, with FormatNumericUnits
    /// / FormatNumericHours / FormatNumericMinutes / FormatNumericSeconds folded
    /// in) as a typed part list; `format` is this joined, `formatToParts` is this
    /// wrapped. The third tuple slot is the part's `unit` field — the SINGULAR
    /// unit identifier, empty for the pieces that belong to no unit (the ":"
    /// separators and the list literals).
    ///
    /// Every numeric run is produced by a real `Intl.NumberFormat` built from the
    /// spec's nfOpts, and the runs are joined by the real ListFormat pattern for
    /// `{type:"unit", style:listStyle}`. Delegating rather than re-deriving is
    /// load-bearing: test262 grades DurationFormat against exactly those two
    /// services (`harness/testIntl.js` re-implements this algorithm on top of
    /// them), so any label or separator zipp lacks cancels on both sides.
    pub(crate) fn duration_format_parts(
        &mut self,
        resolved: u32,
        dur: &[f64; 10],
    ) -> Result<Vec<(String, String, &'static str)>, Thrown> {
        /// The `unit` identifier NumberFormat takes for each Duration field, in
        /// DURATION_FIELDS order (`years` → `year`).
        const SINGULAR: [&str; 10] = [
            "year", "month", "week", "day", "hour", "minute", "second", "millisecond",
            "microsecond", "nanosecond",
        ];
        let locale = self.intl_slot(resolved, "locale");
        let ns = self.display(self.intl_slot(resolved, "numberingSystem"));
        let base_style = self.display(self.intl_slot(resolved, "style"));
        let frac_slot = self.intl_slot(resolved, "fractionalDigits");
        let frac = frac_slot.is_number().then(|| frac_slot.as_f64() as i64);
        // The duration record holds ℝ(field): ToIntegerIfIntegral maps -0 to +0,
        // so `format({years:-0})` must print "0 yr", not "-0 yr"
        // (`format/negative-zero.js` compares it against +0's output).
        let d: [f64; 10] = dur.map(|v| if v == 0.0 { 0.0 } else { v });

        // One entry per unit that gets displayed; the numeric hh:mm:ss run
        // appends into the entry the first of its units opened.
        let mut elements: Vec<Vec<(String, String, &'static str)>> = vec![];
        let mut need_separator = false;
        // "signDisplayed": only the FIRST displayed unit carries the sign; every
        // later one is formatted with signDisplay "never" so "-1 hr, 2 min" does
        // not become "-1 hr, -2 min".
        let mut sign_displayed = true;

        for i in 0..10 {
            let unit_key = native::DURATION_FIELDS[i];
            let nf_unit = SINGULAR[i];
            let style = self.display(self.intl_slot(resolved, unit_key));
            let display = self.display(self.intl_slot(resolved, &format!("{unit_key}Display")));
            let mut opts = ObjMap::new();
            let mut value = d[i];
            // A decimal string when the exact sum needs more precision than f64
            // addition would keep; see duration_fractional_decimal.
            let mut exact: Option<String> = None;
            let mut done = false;
            // Steps 9.g/j: when the NEXT finer unit is "numeric" it is not a unit
            // of its own — it is this unit's fraction, and the loop stops here.
            if (6..=8).contains(&i) {
                let next = self.display(self.intl_slot(resolved, native::DURATION_FIELDS[i + 1]));
                if next == "numeric" {
                    let exponent = [9u32, 6, 3][i - 6];
                    exact = duration_fractional_decimal(&d, exponent);
                    opts.set("maximumFractionDigits", Value::num(frac.unwrap_or(9) as f64));
                    opts.set("minimumFractionDigits", Value::num(frac.unwrap_or(0) as f64));
                    let tv = self.alloc_str("trunc".to_string());
                    opts.set("roundingMode", tv);
                    done = true;
                }
            }
            // Step 9.f: zero minutes still print inside a numeric run when a
            // seconds field will follow them ("1:00:30", not "1:30").
            let mut display_required = false;
            if i == 5 && need_separator {
                display_required =
                    self.display(self.intl_slot(resolved, "secondsDisplay")) == "always"
                        || d[6..].iter().any(|&v| v != 0.0);
            }
            let is_zero = exact.is_none() && value == 0.0;
            if is_zero && display == "auto" && !display_required {
                if done {
                    break;
                }
                continue;
            }
            if sign_displayed {
                sign_displayed = false;
                // The sign has to survive even when the unit that carries it is
                // itself zero ("-0:00:01" for {hours:0, seconds:-1}), and the
                // only value PartitionNumberPattern prints a minus for is -0.
                if is_zero && d.iter().any(|&v| v < 0.0) {
                    value = -0.0;
                }
            } else {
                let nv = self.alloc_str("never".to_string());
                opts.set("signDisplay", nv);
            }
            let nsv = self.alloc_str(ns.clone());
            opts.set("numberingSystem", nsv);
            if style == "2-digit" {
                opts.set("minimumIntegerDigits", Value::num(2.0));
            }
            if style != "numeric" && style != "2-digit" {
                let sv = self.alloc_str("unit".to_string());
                opts.set("style", sv);
                let uv = self.alloc_str(nf_unit.to_string());
                opts.set("unit", uv);
                let dv = self.alloc_str(style.clone());
                opts.set("unitDisplay", dv);
            } else {
                // A numeric hh:mm:ss run never groups: "1234567:20:45".
                opts.set("useGrouping", Value::bool(false));
            }
            let opts_v = Value::heap(self.heap.alloc(HeapObj::Object(Box::new(opts))));
            let nf = self.make_intl(native::INTL_NUMBERFORMAT, locale, opts_v)?;
            let nf_resolved = match self.heap.get(nf.heap_index()) {
                HeapObj::Intl { resolved, .. } => *resolved,
                _ => return Err(Thrown("TypeError: NumberFormat construction failed".into())),
            };
            // The exact decimal goes through ToIntlMathematicalValue the same way
            // a caller's string would, so DurationFormat is never more (or less)
            // precise than the NumberFormat it delegates to.
            let n = match exact {
                Some(s) => {
                    let sv = self.alloc_str(s);
                    self.to_number_coerce(sv)?
                }
                None => value,
            };
            let mut run: Vec<(String, String, &'static str)> =
                self.nf_parts(nf_resolved, n)?.into_iter().map(|(t, v)| (t, v, nf_unit)).collect();
            match elements.last_mut() {
                Some(last) if need_separator => {
                    // [[HoursMinutesSeparator]] / [[MinutesSecondsSeparator]]: a
                    // literal that belongs to no unit, so it carries no `unit`.
                    last.push(("literal".to_string(), ":".to_string(), ""));
                    last.append(&mut run);
                }
                _ => {
                    if style == "2-digit" || style == "numeric" {
                        need_separator = true;
                    }
                    elements.push(run);
                }
            }
            if done {
                break;
            }
        }

        // Steps 11-18: join the elements with ListFormat's `unit` type. "digital"
        // is not a ListFormat style, so it maps to "short".
        let list_style = if base_style == "digital" { "short" } else { base_style.as_str() };
        let strings: Vec<String> = elements
            .iter()
            .map(|p| p.iter().map(|(_, v, _)| v.as_str()).collect::<String>())
            .collect();
        let mut out: Vec<(String, String, &'static str)> = vec![];
        let mut runs = elements.into_iter();
        for (ty, v) in list_parts_en(&strings, "unit", list_style) {
            if ty == "element" {
                out.extend(runs.next().unwrap_or_default());
            } else {
                out.push((ty.to_string(), v, ""));
            }
        }
        Ok(out)
    }

    /// The resolved components as CLDR pattern letters, plus the hour cycle.
    /// This is the request `dtf_pattern::best_pattern` matches against
    /// `availableFormats`; `mask` restricts it to the fields this argument has
    /// (a Temporal.PlainDate contributes no hour, so no hour is requested).
    fn dtf_request(&self, resolved: u32, mask: u16) -> (dtf_pattern::Request, bool) {
        let slot = |k: &str| -> Option<String> {
            match self.heap.get(resolved) {
                HeapObj::Object(m) => m.pos(k).map(|i| self.display(m.vals[i])),
                _ => None,
            }
        };
        let hc = slot("hourCycle").unwrap_or_else(|| "h12".to_string());
        let hour12 = hc == "h11" || hc == "h12";
        let mut req = dtf_pattern::Request::default();
        let has = |bit: u16| mask & bit != 0;
        // Table 7's widths, spelled as UTS #35 field counts: `MMMM` is a wide
        // month, `MMM` an abbreviated one, `MM` two digits, `M` numeric.
        if has(Self::F_ERA) {
            req.push('G', match slot("era").as_deref() {
                Some("long") => 4,
                Some("narrow") => 5,
                _ => 1,
            });
        }
        if has(Self::F_YEAR) {
            req.push('y', if slot("year").as_deref() == Some("2-digit") { 2 } else { 1 });
        }
        if has(Self::F_MONTH) {
            req.push('M', match slot("month").as_deref() {
                Some("2-digit") => 2,
                Some("short") => 3,
                Some("long") => 4,
                Some("narrow") => 5,
                _ => 1,
            });
        }
        if has(Self::F_DAY) {
            req.push('d', if slot("day").as_deref() == Some("2-digit") { 2 } else { 1 });
        }
        if has(Self::F_WEEKDAY) {
            req.push('E', match slot("weekday").as_deref() {
                Some("long") => 4,
                Some("narrow") => 5,
                _ => 3,
            });
        }
        if has(Self::F_DAYPERIOD) {
            req.push('B', match slot("dayPeriod").as_deref() {
                Some("long") => 4,
                Some("narrow") => 5,
                _ => 1,
            });
        }
        if has(Self::F_HOUR) {
            let letter = match hc.as_str() {
                "h11" => 'K',
                "h23" => 'H',
                "h24" => 'k',
                _ => 'h',
            };
            req.push(letter, if slot("hour").as_deref() == Some("2-digit") { 2 } else { 1 });
        }
        if has(Self::F_MINUTE) {
            req.push('m', if slot("minute").as_deref() == Some("2-digit") { 2 } else { 1 });
        }
        if has(Self::F_SECOND) {
            req.push('s', if slot("second").as_deref() == Some("2-digit") { 2 } else { 1 });
        }
        if has(Self::F_FRAC) {
            let n = slot("fractionalSecondDigits").and_then(|s| s.parse::<usize>().ok());
            req.push('S', n.unwrap_or(3).clamp(1, 3));
        }
        if has(Self::F_ZONE) {
            let (c, n) = match slot("timeZoneName").as_deref() {
                Some("long") => ('z', 4),
                Some("shortOffset") => ('O', 1),
                Some("longOffset") => ('O', 4),
                Some("shortGeneric") => ('v', 1),
                Some("longGeneric") => ('v', 4),
                // `timeStyle: "full"` carries `zzzz`, `"long"` carries `z`; with
                // no timeZoneName option at all those are the only two shapes.
                _ if slot("timeZoneName").is_none()
                    && slot("timeStyle").as_deref() == Some("full") => ('z', 4),
                _ => ('z', 1),
            };
            req.push(c, n);
        }
        (req, hour12)
    }

    /// Keep only the pattern items whose field this argument actually has, and
    /// drop the separators the removed fields owned.
    ///
    /// ECMA-402's Temporal integration builds a SEPARATE pattern per Temporal
    /// type by intersecting the resolved components with that type's fields, so
    /// a `dateStyle: "full"` PlainMonthDay must print "March 4" — the CLDR full
    /// date pattern `EEEE, MMMM d, y` with the weekday, the year, and their
    /// commas gone (`temporal-plainmonthday-formatting-datetime-style.js`).
    /// A removed field takes the literal AFTER it, or, when nothing follows,
    /// the literal before it.
    fn dtf_filter(items: Vec<dtf_pattern::Item>, keep: &dyn Fn(char) -> bool) -> Vec<dtf_pattern::Item> {
        let n = items.len();
        let mut drop = vec![false; n];
        for (i, it) in items.iter().enumerate() {
            let dtf_pattern::Item::Field(c, _) = it else { continue };
            if keep(*c) {
                continue;
            }
            drop[i] = true;
            let later = items[i + 1..]
                .iter()
                .any(|x| matches!(x, dtf_pattern::Item::Field(c, _) if keep(*c)));
            if later {
                if let Some(j) = (i + 1..n)
                    .find(|j| matches!(items[*j], dtf_pattern::Item::Lit(_)) && !drop[*j])
                {
                    if j == i + 1 {
                        drop[j] = true;
                    }
                }
            } else if i > 0 && matches!(items[i - 1], dtf_pattern::Item::Lit(_)) {
                drop[i - 1] = true;
            }
        }
        items.into_iter().zip(drop).filter(|(_, d)| !d).map(|(it, _)| it).collect()
    }

    /// FormatDateTimePattern (ECMA-402 §11.5.6) over the CLDR `en` patterns in
    /// `cldr_en`, as a typed part list. `format` is this joined; `formatToParts`
    /// is this wrapped.
    ///
    /// The pattern is chosen first — from `dateStyle`/`timeStyle` when they are
    /// present, otherwise by matching the requested components against CLDR's
    /// `availableFormats` — and then interpreted field by field. Before this
    /// engine had the `en` tables it emitted a fixed `M/D/Y, HH:MM:SS`, which no
    /// month name, weekday name, era or `dateStyle` could ever come out of.
    pub(crate) fn dtf_parts(
        &self,
        resolved: u32,
        ms: f64,
        fields: u16,
        absolute: bool,
    ) -> Vec<(&'static str, String)> {
        let (d, t, glue) = self.dtf_pattern_halves(resolved, fields);
        let items = match (d.is_empty(), t.is_empty()) {
            (false, false) => dtf_pattern::splice_glue(cldr_en::DATETIME_GLUE_AT[glue], &d, &t),
            (false, true) => d,
            _ => t,
        };
        self.dtf_render(resolved, ms, absolute, &items)
    }

    /// The pattern for `fields`, split into literals and fields — and kept as
    /// two HALVES, because `formatRange` needs them apart: when only the time
    /// differs, CLDR prints the date once and ranges the time inside it
    /// ("8/4/2021, 12:30:45 AM – 11:30:45 PM").
    fn dtf_pattern_halves(
        &self,
        resolved: u32,
        fields: u16,
    ) -> (Vec<dtf_pattern::Item>, Vec<dtf_pattern::Item>, usize) {
        let slot = |k: &str| -> Option<String> {
            match self.heap.get(resolved) {
                HeapObj::Object(m) => m.pos(k).map(|i| self.display(m.vals[i])),
                _ => None,
            }
        };
        let (req, hour12) = self.dtf_request(resolved, fields);
        // Which CLASSES survive: a field the pattern carries but this argument
        // does not have is dropped by `dtf_filter`.
        let has = |bit: u16| fields & bit != 0;
        let keep = move |c: char| match c {
            'G' => has(Self::F_ERA),
            'y' | 'Y' | 'u' => has(Self::F_YEAR),
            'M' | 'L' => has(Self::F_MONTH),
            'd' => has(Self::F_DAY),
            'E' | 'e' | 'c' => has(Self::F_WEEKDAY),
            // The day period rides with the hour: `h:mm a` keeps its "a" as long
            // as the hour is printed on a 12-hour cycle, and `{minute, second}`
            // never gains one. On h23/h24 it goes, with its separator.
            'a' | 'b' | 'B' => (hour12 && has(Self::F_HOUR)) || has(Self::F_DAYPERIOD),
            'h' | 'H' | 'K' | 'k' => has(Self::F_HOUR),
            'm' => has(Self::F_MINUTE),
            's' => has(Self::F_SECOND),
            'S' => has(Self::F_FRAC),
            'z' | 'Z' | 'O' | 'v' | 'V' | 'X' | 'x' => has(Self::F_ZONE),
            _ => true,
        };
        // dateStyle/timeStyle name CLDR's four stored patterns directly; there
        // is no skeleton matching for them (ECMA-402 DateTimeStylePattern).
        let ds = slot("dateStyle").and_then(|s| dtf_pattern::style_index(&s));
        let ts = slot("timeStyle").and_then(|s| dtf_pattern::style_index(&s));
        if ds.is_some() || ts.is_some() {
            let dpat = ds.map(|i| cldr_en::DATE_FORMATS[i].to_string());
            let tpat = ts.map(|i| cldr_en::TIME_FORMATS[i].to_string());
            let d_items = dpat.map(|p| Self::dtf_filter(dtf_pattern::parse_pattern(&p), &keep));
            let t_items = tpat.map(|p| {
                // `en`'s four stored time patterns are 12-hour (`h:mm:ss a …`).
                // An h23/h24 request rewrites the hour field to the padded
                // 24-hour form CLDR uses for it; the day period is then dropped
                // by `keep` above, taking its separator with it.
                let items = dtf_pattern::parse_pattern(&p)
                    .into_iter()
                    .map(|it| match it {
                        dtf_pattern::Item::Field('h', _) if !hour12 => {
                            let hc = slot("hourCycle").unwrap_or_default();
                            dtf_pattern::Item::Field(if hc == "h24" { 'k' } else { 'H' }, 2)
                        }
                        other => other,
                    })
                    .collect();
                Self::dtf_filter(items, &keep)
            });
            // CLDR 42+ keeps a second "at time" glue for exactly this
            // combination; ICU uses it, so `dateStyle: "long"` +
            // `timeStyle: "short"` reads "May 1, 1886 at 2:12 PM".
            return (d_items.unwrap_or_default(), t_items.unwrap_or_default(), ds.unwrap_or(3));
        }
        let (dpat, tpat, glue) = dtf_pattern::best_pattern_halves(&req, hour12);
        (
            Self::dtf_filter(dtf_pattern::parse_pattern(&dpat), &keep),
            Self::dtf_filter(dtf_pattern::parse_pattern(&tpat), &keep),
            glue,
        )
    }

    /// Interpret a parsed pattern against one instant.
    fn dtf_render(
        &self,
        resolved: u32,
        ms: f64,
        absolute: bool,
        items: &[dtf_pattern::Item],
    ) -> Vec<(&'static str, String)> {
        let slot = |k: &str| -> Option<String> {
            match self.heap.get(resolved) {
                HeapObj::Object(m) => m.pos(k).map(|i| self.display(m.vals[i])),
                _ => None,
            }
        };
        // The formatter's zone shifts the instant to a local wall clock. For a
        // named IANA zone that shift depends on the instant itself, so it is
        // looked up at `ms` rather than being a per-zone constant.
        //
        // A PLAIN Temporal value is not an instant: the spec converts its wall
        // clock to an epoch through the formatter's zone and straight back, so
        // the offset cancels and its own fields print unchanged
        // (`temporal-objects-resolved-time-zone.js`).
        let tz_minutes = slot("timeZone")
            .as_deref()
            .and_then(|tz| time_zone_offset_minutes_at(tz, ms as i128))
            .unwrap_or(0);
        let offset_ms = if absolute { tz_minutes as i128 * 60_000 } else { 0 };
        let total_ms = ms as i128 + offset_ms;
        let days = total_ms.div_euclid(86_400_000) as i64;
        let (y, mo, d) = epoch_days_to_iso(days);
        let rem_ns = total_ms.rem_euclid(86_400_000) * 1_000_000;
        let t = ns_to_time(rem_ns); // [h, mi, s, ms, us, ns]
        // 1970-01-01 was a Thursday, index 4 in CLDR's Sunday-first week.
        let weekday = (days.rem_euclid(7) + 4) as usize % 7;
        // Proleptic Gregorian has no year 0: CLDR year 1 BC is ISO year 0, so the
        // ERA YEAR is 1 - y below the epoch (`proleptic-gregorian-calendar.js`).
        let (era_idx, era_year) = if y <= 0 { (0usize, 1 - y) } else { (1usize, y) };
        let minutes_of_day = (t[0] * 60 + t[1]) as i32;
        let mut out: Vec<(&'static str, String)> = vec![];
        for item in items {
            match item {
                dtf_pattern::Item::Lit(s) => out.push(("literal", s.clone())),
                dtf_pattern::Item::Field(c, n) => {
                    let n = *n;
                    // CLDR width index: 0 = wide, 1 = abbreviated, 2 = narrow.
                    let text_width = |n: usize| match n {
                        4 => 0,
                        5 => 2,
                        _ => 1,
                    };
                    match c {
                        'G' => out.push((
                            "era",
                            match text_width(n) {
                                0 => cldr_en::ERAS_WIDE[era_idx],
                                2 => cldr_en::ERAS_NARROW[era_idx],
                                _ => cldr_en::ERAS_ABBR[era_idx],
                            }
                            .to_string(),
                        )),
                        'y' | 'Y' | 'u' => {
                            let v = if *c == 'u' { y } else { era_year };
                            // `yy` is the last two digits, zero-padded; any other
                            // count is a minimum width.
                            let s = if n == 2 {
                                format!("{:02}", v.rem_euclid(100))
                            } else {
                                format!("{:0width$}", v, width = n)
                            };
                            out.push(("year", s));
                        }
                        'M' | 'L' => {
                            let i = (mo - 1) as usize;
                            let s = match n {
                                1 => mo.to_string(),
                                2 => format!("{mo:02}"),
                                3 => cldr_en::MONTHS_ABBR[i].to_string(),
                                5 => cldr_en::MONTHS_NARROW[i].to_string(),
                                _ => cldr_en::MONTHS_WIDE[i].to_string(),
                            };
                            out.push(("month", s));
                        }
                        'd' => out.push(("day", format!("{:0width$}", d, width = n))),
                        'E' | 'e' | 'c' => {
                            let s = match n {
                                4 => cldr_en::DAYS_WIDE[weekday],
                                5 => cldr_en::DAYS_NARROW[weekday],
                                6 => cldr_en::DAYS_SHORT[weekday],
                                _ => cldr_en::DAYS_ABBR[weekday],
                            };
                            out.push(("weekday", s.to_string()));
                        }
                        'a' | 'b' => {
                            let key = if t[0] < 12 { "am" } else { "pm" };
                            out.push((
                                "dayPeriod",
                                dtf_pattern::day_period_name(key, text_width(n)).to_string(),
                            ));
                        }
                        'B' => {
                            let key = dtf_pattern::day_period_key(minutes_of_day);
                            out.push((
                                "dayPeriod",
                                dtf_pattern::day_period_name(key, text_width(n)).to_string(),
                            ));
                        }
                        'h' | 'H' | 'K' | 'k' => {
                            let h24 = t[0];
                            let v = match c {
                                'h' => {
                                    if h24 % 12 == 0 {
                                        12
                                    } else {
                                        h24 % 12
                                    }
                                }
                                'K' => h24 % 12,
                                'k' => {
                                    if h24 == 0 {
                                        24
                                    } else {
                                        h24
                                    }
                                }
                                _ => h24,
                            };
                            out.push(("hour", format!("{:0width$}", v, width = n)));
                        }
                        'm' => out.push(("minute", format!("{:0width$}", t[1], width = n))),
                        's' => out.push(("second", format!("{:0width$}", t[2], width = n))),
                        'S' => {
                            // fractionalSecond is truncated, never rounded.
                            let ms_str = format!("{:03}", t[3]);
                            let mut s: String = ms_str.chars().take(n).collect();
                            while s.len() < n {
                                s.push('0');
                            }
                            out.push(("fractionalSecond", s));
                        }
                        'z' | 'Z' | 'O' | 'v' | 'V' | 'X' | 'x' => {
                            out.push(("timeZoneName", self.dtf_zone_name(resolved, *c, n, tz_minutes)));
                        }
                        // A field this engine does not implement (quarter, week
                        // of year, …) is unreachable from ECMA-402's Table 7.
                        _ => {}
                    }
                }
            }
        }
        // The date-time NUMBERS follow the resolved numbering system too
        // (`format/numbering-system.js`); the literals between them do not.
        let ns = slot("numberingSystem").unwrap_or_else(|| "latn".to_string());
        if ns != "latn" {
            for (ty, v) in out.iter_mut() {
                if *ty != "literal" && *ty != "timeZoneName" && *ty != "dayPeriod" {
                    *v = translate_digits(v, &ns);
                }
            }
        }
        out
    }

    /// The zone name a `z`/`v`/`O` field prints.
    ///
    /// There is no CLDR zone-name data here, and UTS #35 §4.5 makes the
    /// *localized GMT format* the specified fallback for exactly that case, so
    /// the name is rendered from the offset this formatter actually used —
    /// self-consistent with the rest of the pattern rather than an invented name.
    ///
    /// The ONE zone that does get its CLDR name is "UTC": `en` gives Etc/UTC the
    /// short name "UTC" and the long name "Coordinated Universal Time", and that
    /// is not interchangeable with the GMT fallback — an OFFSET zone of `+00:00`
    /// still prints "GMT" (`ZonedDateTime/…/toLocaleString/offset-time-zones.js`
    /// asserts the GMT spelling, `…/default-includes-time-and-time-zone-name.js`
    /// the UTC one). The *Offset and *Generic styles stay on the GMT format for
    /// UTC too.
    fn dtf_zone_name(&self, resolved: u32, c: char, n: usize, tz_minutes: i64) -> String {
        let slot = |k: &str| -> Option<String> {
            match self.heap.get(resolved) {
                HeapObj::Object(m) => m.pos(k).map(|i| self.display(m.vals[i])),
                _ => None,
            }
        };
        let m = tz_minutes;
        let utc_named = slot("timeZone").as_deref() == Some("UTC") && c == 'z';
        if utc_named {
            return if n >= 4 { "Coordinated Universal Time" } else { "UTC" }.to_string();
        }
        if m == 0 {
            return "GMT".to_string();
        }
        let sign = if m < 0 { '-' } else { '+' };
        let (h, mi) = (m.abs() / 60, m.abs() % 60);
        if mi != 0 { format!("GMT{sign}{h}:{mi:02}") } else { format!("GMT{sign}{h}") }
    }

    /// Intl.DateTimeFormat.prototype.format(date) — UTC, en-US conventions.
    pub(crate) fn dtf_format(&self, resolved: u32, ms: f64, fields: u16, absolute: bool) -> String {
        self.dtf_parts(resolved, ms, fields, absolute).into_iter().map(|(_, v)| v).collect()
    }

    /// `Temporal.<Type>.prototype.toLocaleString(locales, options)` for every
    /// Temporal type (ECMA-402's Temporal integration). Each type builds an
    /// `Intl.DateTimeFormat` with ITS OWN required/defaults pair and then runs
    /// FormatDateTime over the receiver — which is why `pd.toLocaleString("en")`
    /// prints a date, `pt.toLocaleString("en")` a time, and `zdt.toLocaleString`
    /// both plus a zone name. Temporal.Duration goes to Intl.DurationFormat
    /// instead (it is not a date-time at all).
    pub(crate) fn temporal_to_locale_string(
        &mut self,
        this: Value,
        kind: u8,
        locales: Value,
        options: Value,
    ) -> Result<Value, Thrown> {
        if kind == 0 {
            // sec-temporal.duration.prototype.tolocalestring: construct an
            // Intl.DurationFormat (so the options are validated exactly as the
            // constructor would) and format the receiver with it.
            let df = self.make_intl(native::INTL_DURATIONFORMAT, locales, options)?;
            let resolved = self.intl_this(df, native::INTL_DURATIONFORMAT, "toLocaleString")?;
            let dur = self.to_duration_f64(this)?;
            let parts = self.duration_format_parts(resolved, &dur)?;
            let s: String = parts.into_iter().map(|(_, v, _)| v).collect();
            return Ok(self.alloc_str(s));
        }
        let mode = match kind {
            1 | 5 | 6 => DtfDefaults::Date, // PlainDate / PlainYearMonth / PlainMonthDay
            2 => DtfDefaults::Time,         // PlainTime
            7 => DtfDefaults::Zoned,        // ZonedDateTime
            _ => DtfDefaults::All,          // PlainDateTime / Instant
        };
        // ZonedDateTime formats in ITS OWN zone, so a `timeZone` option would be
        // a second, conflicting answer to the same question: the spec rejects it
        // outright, even when it agrees (`toLocaleString/options-timeZone.js`).
        let zdt_zone = if kind == 7 {
            if options != Value::UNDEFINED {
                self.require_object_coercible(options)?;
                let o = self.to_object(options)?;
                if self.get_prop(o, "timeZone")? != Value::UNDEFINED {
                    return Err(Thrown(
                        "TypeError: ZonedDateTime.toLocaleString does not accept a timeZone option"
                            .into(),
                    ));
                }
            }
            Some(self.zdt_tz_id(this.heap_index()).unwrap_or_else(|| "UTC".to_string()))
        } else {
            None
        };
        let dtf = self.make_intl_dtf(native::INTL_DATETIMEFORMAT, locales, options, mode)?;
        let resolved = self.intl_this(dtf, native::INTL_DATETIMEFORMAT, "toLocaleString")?;
        if let Some(tz) = zdt_zone {
            // Step "CreateDataPropertyOrThrow(optionsCopy, "timeZone",
            // zonedDateTime.[[TimeZone]])" — done on the resolved record, since
            // the constructor above already validated everything else.
            let v = self.alloc_str(tz);
            if let HeapObj::Object(m) = self.heap.get_mut(resolved) {
                m.set("timeZone", v);
            }
        }
        self.dtf_check_calendar(resolved, this, "toLocaleString")?;
        let (fields, absolute) = self.dtf_fields_for_kind(resolved, kind, "toLocaleString")?;
        let ms = if kind == 7 {
            (self.zdt_epoch_ns(this.heap_index()).unwrap_or(0) / 1_000_000) as f64
        } else {
            self.dtf_time_value(this)?
        };
        let s = self.dtf_format(resolved, ms, fields, absolute);
        Ok(self.alloc_str(s))
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

/// CreateDateTimeFormat's `required`/`defaults` pair. `required` decides which
/// options clear needDefaults (and which style is outright rejected); `defaults`
/// decides which components are filled in when none did.
///
/// Every entry point picks its own pair: `Intl.DateTimeFormat` is (any, date),
/// `Date.prototype.toLocaleString` is (any, all), `toLocaleDateString` is
/// (date, date), and each `Temporal.*.prototype.toLocaleString` uses the group
/// its own type has.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DtfDefaults {
    /// (any, date) — the `Intl.DateTimeFormat` constructor itself.
    Standard,
    /// (date, date) — toLocaleDateString, PlainDate, PlainYearMonth, PlainMonthDay.
    Date,
    /// (time, time) — toLocaleTimeString, PlainTime.
    Time,
    /// (any, all) — Date.prototype.toLocaleString, PlainDateTime, Instant.
    All,
    /// (any, all + timeZoneName "short") — ZonedDateTime.
    Zoned,
}

impl DtfDefaults {
    /// (date half required, time half required) — which component groups clear
    /// needDefaults, and which style the formatter accepts at all.
    fn required(self) -> (bool, bool) {
        match self {
            DtfDefaults::Date => (true, false),
            DtfDefaults::Time => (false, true),
            _ => (true, true),
        }
    }

    /// (fill year/month/day, fill hour/minute/second, fill timeZoneName).
    fn defaults(self) -> (bool, bool, bool) {
        match self {
            DtfDefaults::Standard | DtfDefaults::Date => (true, false, false),
            DtfDefaults::Time => (false, true, false),
            DtfDefaults::All => (true, true, false),
            DtfDefaults::Zoned => (true, true, true),
        }
    }
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
/// ECMA-402 Table 10, "Numbering systems with simple digit mappings": the code
/// point of DIGIT ZERO in each system whose ten digits are consecutive. This is
/// a **normative table of the specification**, not CLDR locale data — every
/// implementation must format in all of these, and
/// `Intl.supportedValuesOf("numberingSystem")` must list them all
/// (`numberingSystems-with-simple-digit-mappings.js`). Sorted by name, which is
/// also the order supportedValuesOf must report.
pub(crate) const NUMBERING_SYSTEM_ZERO: &[(&str, u32)] = &[
    ("adlm", 0x1E950),
    ("ahom", 0x11730),
    ("arab", 0x660),
    ("arabext", 0x6F0),
    ("bali", 0x1B50),
    ("beng", 0x9E6),
    ("bhks", 0x11C50),
    ("brah", 0x11066),
    ("cakm", 0x11136),
    ("cham", 0xAA50),
    ("deva", 0x966),
    ("diak", 0x11950),
    ("fullwide", 0xFF10),
    ("gara", 0x10D40),
    ("gong", 0x11DA0),
    ("gonm", 0x11D50),
    ("gujr", 0xAE6),
    ("gukh", 0x16130),
    ("guru", 0xA66),
    ("hanidec", 0x3007),
    ("hmng", 0x16B50),
    ("hmnp", 0x1E140),
    ("java", 0xA9D0),
    ("kali", 0xA900),
    ("kawi", 0x11F50),
    ("khmr", 0x17E0),
    ("knda", 0xCE6),
    ("krai", 0x16D70),
    ("lana", 0x1A80),
    ("lanatham", 0x1A90),
    ("laoo", 0xED0),
    ("latn", 0x30),
    ("lepc", 0x1C40),
    ("limb", 0x1946),
    ("mathbold", 0x1D7CE),
    ("mathdbl", 0x1D7D8),
    ("mathmono", 0x1D7F6),
    ("mathsanb", 0x1D7EC),
    ("mathsans", 0x1D7E2),
    ("mlym", 0xD66),
    ("modi", 0x11650),
    ("mong", 0x1810),
    ("mroo", 0x16A60),
    ("mtei", 0xABF0),
    ("mymr", 0x1040),
    ("mymrepka", 0x116DA),
    ("mymrpao", 0x116D0),
    ("mymrshan", 0x1090),
    ("mymrtlng", 0xA9F0),
    ("nagm", 0x1E4F0),
    ("newa", 0x11450),
    ("nkoo", 0x7C0),
    ("olck", 0x1C50),
    ("onao", 0x1E5F1),
    ("orya", 0xB66),
    ("osma", 0x104A0),
    ("outlined", 0x1CCF0),
    ("rohg", 0x10D30),
    ("saur", 0xA8D0),
    ("segment", 0x1FBF0),
    ("shrd", 0x111D0),
    ("sind", 0x112F0),
    ("sinh", 0xDE6),
    ("sora", 0x110F0),
    ("sund", 0x1BB0),
    ("sunu", 0x11BF0),
    ("takr", 0x116C0),
    ("talu", 0x19D0),
    ("tamldec", 0xBE6),
    ("telu", 0xC66),
    ("thai", 0xE50),
    ("tibt", 0xF20),
    ("tirh", 0x114D0),
    ("tnsa", 0x16AC0),
    ("tols", 0x11DE0),
    ("vaii", 0xA620),
    ("wara", 0x118E0),
    ("wcho", 0x1E2F0),
];

/// `hanidec` is the one Table 10 system whose digits are NOT consecutive.
const HANIDEC_DIGITS: [char; 10] =
    ['\u{3007}', '\u{4e00}', '\u{4e8c}', '\u{4e09}', '\u{56db}', '\u{4e94}', '\u{516d}',
     '\u{4e03}', '\u{516b}', '\u{4e5d}'];

pub(crate) const AVAILABLE_NUMBERING_SYSTEMS: &[&str] = &[
    "adlm",
    "ahom",
    "arab",
    "arabext",
    "bali",
    "beng",
    "bhks",
    "brah",
    "cakm",
    "cham",
    "deva",
    "diak",
    "fullwide",
    "gara",
    "gong",
    "gonm",
    "gujr",
    "gukh",
    "guru",
    "hanidec",
    "hmng",
    "hmnp",
    "java",
    "kali",
    "kawi",
    "khmr",
    "knda",
    "krai",
    "lana",
    "lanatham",
    "laoo",
    "latn",
    "lepc",
    "limb",
    "mathbold",
    "mathdbl",
    "mathmono",
    "mathsanb",
    "mathsans",
    "mlym",
    "modi",
    "mong",
    "mroo",
    "mtei",
    "mymr",
    "mymrepka",
    "mymrpao",
    "mymrshan",
    "mymrtlng",
    "nagm",
    "newa",
    "nkoo",
    "olck",
    "onao",
    "orya",
    "osma",
    "outlined",
    "rohg",
    "saur",
    "segment",
    "shrd",
    "sind",
    "sinh",
    "sora",
    "sund",
    "sunu",
    "takr",
    "talu",
    "tamldec",
    "telu",
    "thai",
    "tibt",
    "tirh",
    "tnsa",
    "tols",
    "vaii",
    "wara",
    "wcho"
];

/// Map the ASCII digits of an already-formatted number into `ns`. Everything
/// else (signs, separators, currency symbols) is untouched — the separators are
/// locale data this engine does not have, but the DIGITS are not.
pub(crate) fn translate_digits(s: &str, ns: &str) -> String {
    if ns == "latn" {
        return s.to_string();
    }
    if ns == "hanidec" {
        return s
            .chars()
            .map(|c| {
                if c.is_ascii_digit() { HANIDEC_DIGITS[(c as u8 - b'0') as usize] } else { c }
            })
            .collect();
    }
    let Some(&(_, zero)) = NUMBERING_SYSTEM_ZERO.iter().find(|(n, _)| *n == ns) else {
        return s.to_string();
    };
    s.chars()
        .map(|c| {
            if c.is_ascii_digit() {
                char::from_u32(zero + (c as u32 - '0' as u32)).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

/// ResolveLocale for ONE [[RelevantExtensionKeys]] entry. The *option* wins
/// only when the implementation supports its value; otherwise the `-u-` value
/// wins if IT is supported; otherwise the default. The second result says
/// whether the resolved locale must carry the keyword, which it must exactly
/// when the extension is what supplied the winning value — so `en-u-nu-arab`
/// with `{numberingSystem: "invalid"}` resolves to arab and keeps `-u-nu-arab`,
/// while `en-u-nu-latn` with `{numberingSystem: "arab"}` resolves to arab and
/// reports plain "en" (resolved-*-unicode-extensions-and-options.js).
///
/// An empty `available` means "every well-formed value is supported".
pub(crate) fn resolve_ext_key(
    option: Option<String>,
    ext: Option<String>,
    available: &[&str],
    default: &str,
) -> (String, bool) {
    let ok = |v: &String| available.is_empty() || available.contains(&v.as_str());
    let opt = option.map(|s| s.to_ascii_lowercase()).filter(&ok);
    let ext_ok = ext.map(|s| s.to_ascii_lowercase()).filter(&ok);
    let value = opt.or_else(|| ext_ok.clone()).unwrap_or_else(|| default.to_string());
    let reflect = ext_ok.as_deref() == Some(value.as_str());
    (value, reflect)
}

/// ResolveLocale's [[locale]]: the requested tag reduced to its language id plus
/// `-u-` keywords for exactly the relevant extension keys the extension
/// supplied. Every other keyword — and every attribute — is dropped, which is
/// what `unicode-ext-seq-with-attribute.js` ("de-u-attrval-co-phonebk" resolves
/// to "de-u-co-phonebk") and `ignore-invalid-unicode-ext-values.js` require.
pub(crate) fn resolved_locale_tag(tag: &str, keys: &[(&str, String)]) -> String {
    let Some(mut t) = crate::vm::locale_tag::parse_lang_tag(tag) else {
        return tag.to_string();
    };
    t.u_attributes.clear();
    t.u_keywords.clear();
    t.has_u = false;
    for (k, v) in keys {
        t.set_u(k, Some(v.clone()));
    }
    t.canonical()
}

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
        // Step 1a: the code must match `unicode_language_id` — the language /
        // script / region / variant prefix ONLY. A tag carrying any extension
        // ("en-u-hebrew", "en-x-priv") parses fine as a `unicode_locale_id` but
        // is a RangeError here, so reject anything whose canonical form grows an
        // extension singleton.
        "language" => crate::vm::locale_tag::parse_lang_tag(code)
            .filter(|t| {
                !t.has_u
                    && t.transform.is_empty()
                    && t.other.is_empty()
                    && t.private.is_empty()
                    && t.u_keywords.is_empty()
            })
            .map(|t| t.canonical()),
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
pub(crate) const SANCTIONED_UNITS: &[&str] = &[
    "acre", "bit", "byte", "celsius", "centimeter", "day", "degree", "fluid-ounce", "foot",
    "gallon", "gigabit", "gigabyte", "gram", "hectare", "hour", "inch", "kilobit", "kilobyte",
    "kilogram", "kilometer", "liter", "megabit", "megabyte", "meter", "microsecond", "mile",
    "mile-scandinavian", "milliliter", "millimeter", "millisecond", "minute", "month",
    "nanosecond", "ounce", "percent", "petabyte", "pound", "second", "stone", "terabit",
    "terabyte", "week", "yard", "year", "fahrenheit",
];

/// The compact affix `s` ends with, if any. The affixes are a closed set (the
/// literal halves of the locale's compact-decimal rows), so recognising one is
/// exact rather than a guess — the longest match wins so " million" is not read
/// as "n".
fn compact_affix_of(s: &str, display: &str) -> Option<&'static str> {
    let table = if display == "long" {
        crate::vm::cldr_en::COMPACT_DECIMAL_LONG
    } else {
        crate::vm::cldr_en::COMPACT_DECIMAL_SHORT
    };
    table
        .iter()
        .map(|(.., pat)| pat.trim_start_matches('0'))
        .filter(|aff| !aff.is_empty() && s.ends_with(aff))
        .max_by_key(|aff| aff.len())
}

/// The prefix and suffix of the NEGATIVE subpattern of CLDR's accounting
/// currency format (`en`: `¤#,##0.00;(¤#,##0.00)` → `("(", ")")`). Everything
/// before the first and after the last of the pattern's own symbols (`¤`, `#`,
/// `0`, and the grouping/decimal separators) is affix.
fn accounting_affixes() -> (&'static str, &'static str) {
    let neg = crate::vm::cldr_en::PATTERN_ACCOUNTING.split(';').nth(1).unwrap_or("");
    let body = |c: char| matches!(c, '¤' | '#' | '0' | ',' | '.');
    let start = neg.find(body).unwrap_or(neg.len());
    let end = neg.rfind(body).map_or(neg.len(), |i| i + neg[i..].chars().next().unwrap().len_utf8());
    (&neg[..start], &neg[end..])
}

/// The CLDR `en` pattern that renders `unit` at `width` for the given plural
/// category — `"{0} km/h"`, `"{0}%"`, `"{0} kilometers per hour"` — or None when
/// CLDR has no row for it (the caller then echoes the identifier rather than
/// inventing a name).
///
/// A `<num>-per-<den>` compound follows UTS #35 "Compound Units" in order:
/// CLDR's own idiomatic row first (so `kilometer-per-hour` is "km/h", never
/// "km/hour"), then the DENOMINATOR's `perUnitPattern` wrapped around the
/// numerator, and only then the generic `compoundUnitPattern`.
fn cldr_unit_pattern(unit: &str, width: &str, one: bool) -> Option<String> {
    use crate::vm::cldr_en as d;
    let pick = |row: &(&str, &str, &'static str, &'static str)| {
        if one { row.2.to_string() } else { row.3.to_string() }
    };
    if let Some(r) = d::UNIT_PER_PATTERNS.iter().find(|(u, w, ..)| *u == unit && *w == width) {
        return Some(pick(r));
    }
    if let Some(r) = d::UNIT_PATTERNS.iter().find(|(u, w, ..)| *u == unit && *w == width) {
        return Some(pick(r));
    }
    let (num, den) = unit.split_once("-per-")?;
    let num_pat = d::UNIT_PATTERNS.iter().find(|(u, w, ..)| *u == num && *w == width).map(pick)?;
    if let Some((.., p)) = d::UNIT_PER_UNIT.iter().find(|(u, w, _)| *u == den && *w == width) {
        return Some(p.replace("{0}", &num_pat));
    }
    // The denominator contributes its NAME only: its own pattern minus the
    // `{0}` placeholder and the spacing that separated them.
    let den_pat = d::UNIT_PATTERNS.iter().find(|(u, w, ..)| *u == den && *w == width)?;
    let den_name = den_pat.2.replace("{0}", "");
    let idx = ["long", "short", "narrow"].iter().position(|w| *w == width)?;
    Some(d::UNIT_COMPOUND_PER[idx].replace("{0}", &num_pat).replace("{1}", den_name.trim()))
}

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

/// Accept a time-zone argument for Intl.DateTimeFormat: an offset identifier
/// (`±HH:MM`, canonicalized) or a Zone/Link name from the bundled IANA table,
/// matched ASCII-case-insensitively and returned in its canonical spelling.
///
/// The IDENTIFIER is what comes back, not its primary identifier: ECMA-402
/// CreateDateTimeFormat takes `[[Identifier]]`, so `{timeZone:"Etc/GMT"}`
/// resolves to "Etc/GMT" and `{timeZone:"Europe/Bratislava"}` to
/// "Europe/Bratislava" rather than either being folded onto its Link target.
/// `None` means the identifier is not in the database -> RangeError.
pub(crate) fn canonicalize_time_zone(s: &str) -> Option<String> {
    // An offset time zone identifier, checked BEFORE the name lookup so a
    // malformed one ("+3", "-2400") is rejected rather than looked up.
    if s.starts_with('+') || s.starts_with('-') {
        return offset_time_zone_minutes(s).map(format_offset_time_zone);
    }
    crate::vm::temporal::tzdb::lookup(s).map(|z| z.canonical.to_string())
}

/// Parse an offset time zone identifier to signed minutes, or `None` if it is
/// not one. The grammar is `UTCOffset[~SubMinutePrecision]` (ECMA-402
/// IsOffsetTimeZoneIdentifier): a sign, then `HH`, `HHMM` or `HH:MM` — nothing
/// else. Seconds, a lone-digit hour, a 3-digit run and an out-of-range field are
/// all rejected, which is the whole point of `constructor-invalid-offset-timezone`
/// ("+3", "-014", "-2400", "+15:59:00" … must each be a RangeError).
pub(crate) fn offset_time_zone_minutes(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.is_empty() || !(b[0] == b'+' || b[0] == b'-') {
        return None;
    }
    let sign: i64 = if b[0] == b'-' { -1 } else { 1 };
    let body = &s[1..];
    let (hh, mm) = match body.len() {
        2 => (body, "00"),
        4 => (&body[..2], &body[2..]),
        5 if body.as_bytes()[2] == b':' => (&body[..2], &body[3..]),
        _ => return None,
    };
    if !hh.bytes().chain(mm.bytes()).all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (h, m): (i64, i64) = (hh.parse().ok()?, mm.parse().ok()?);
    if h > 23 || m > 59 {
        return None;
    }
    Some(sign * (h * 60 + m))
}

/// FormatOffsetTimeZoneIdentifier: always `±HH:MM`, and always `+` for zero —
/// `-00` and `-00:00` both canonicalize to `+00:00`.
fn format_offset_time_zone(minutes: i64) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let a = minutes.abs();
    format!("{sign}{:02}:{:02}", a / 60, a % 60)
}

/// The UTC offset a time zone identifier has AT an instant, in minutes: fixed
/// for an offset identifier, and the tz database's answer for a named zone.
/// `ms` is the epoch milliseconds being formatted, which is what makes
/// `{timeZone:"America/New_York"}` print EDT in July and EST in January.
pub(crate) fn time_zone_offset_minutes_at(tz: &str, ms: i128) -> Option<i64> {
    if let Some(m) = offset_time_zone_minutes(tz) {
        return Some(m);
    }
    let z = crate::vm::temporal::tzdb::lookup(tz)?;
    Some(crate::vm::temporal::tzdb::offset_seconds(z.zone, ms.div_euclid(1000) as i64) as i64 / 60)
}

/// AvailableCanonicalTimeZones: the primary identifiers of the bundled IANA
/// table, sorted and unique. "Etc/UTC" and "Etc/GMT" are absent because both
/// carry the primary identifier "UTC" (ECMA-402
/// sec-availablenamedtimezoneidentifiers step 5.c).
pub(crate) fn available_time_zones() -> Vec<String> {
    crate::vm::temporal::tzdb::primary_ids().into_iter().map(str::to_string).collect()
}
