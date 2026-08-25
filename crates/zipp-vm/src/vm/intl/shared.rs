#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;
use crate::vm::*;
use crate::vm::{cldr_en, dtf_pattern};
use std::collections::HashSet;

fn account_locale_list_bytes(total: &mut usize, additional: usize) -> Result<(), Thrown> {
    *total = total
        .checked_add(additional)
        .filter(|&bytes| bytes <= MAX_STRING_BYTES)
        .ok_or_else(|| Thrown("RangeError: locale list text limit exceeded".into()))?;
    Ok(())
}

fn push_unique_locale(
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
    retained_bytes: &mut usize,
    tag: String,
) -> Result<(), Thrown> {
    if seen.contains(&tag) {
        return Ok(());
    }
    account_locale_list_bytes(retained_bytes, tag.len())?;
    out.try_reserve(1)
        .map_err(|_| Thrown("RangeError: locale list allocation failed".into()))?;
    seen.try_reserve(1)
        .map_err(|_| Thrown("RangeError: locale list allocation failed".into()))?;

    // Keep the ordered output and the membership index separately without an
    // infallible String::clone allocation.
    let mut membership_key = String::new();
    membership_key
        .try_reserve_exact(tag.len())
        .map_err(|_| Thrown("RangeError: locale list allocation failed".into()))?;
    membership_key.push_str(&tag);
    seen.insert(membership_key);
    out.push(tag);
    Ok(())
}

impl<'p> Vm<'p> {
    /// Copy one ListFormat element into a native temporary while bounding the
    /// aggregate text retained by the whole list. Reusing the same near-limit
    /// guest string many times must not turn into one native clone per element.
    fn push_string_list_value(
        &mut self,
        out: &mut Vec<String>,
        total_bytes: &mut usize,
        value: Value,
    ) -> Result<(), Thrown> {
        if !(value.is_heap() && self.heap.is_str_like(value.heap_index())) {
            return Err(Thrown("TypeError: list elements must be strings".into()));
        }
        let idx = value.heap_index();
        self.heap.flatten(idx);
        let byte_len = match self.heap.get(idx) {
            HeapObj::Str(js) => js.as_bytes().len(),
            _ => unreachable!("flattened string remains string-like"),
        };
        let next_total = total_bytes
            .checked_add(byte_len)
            .filter(|&n| n <= MAX_STRING_BYTES)
            .ok_or_else(|| Thrown("RangeError: Invalid string length".into()))?;
        self.preflight_guest_string_size(next_total)?;
        out.try_reserve(1)
            .map_err(|_| Thrown("RangeError: list allocation failed".into()))?;
        let text = self.heap.str_cow(idx).expect("validated string-like value");
        let mut owned = String::new();
        owned
            .try_reserve_exact(text.len())
            .map_err(|_| Thrown("RangeError: string allocation failed".into()))?;
        owned.push_str(&text);
        out.push(owned);
        *total_bytes = next_total;
        Ok(())
    }

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
        Err(Thrown(format!(
            "TypeError: {m} called on an incompatible receiver"
        )))
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
        if let HeapObj::Intl {
            kind: native::INTL_LOCALE,
            resolved,
        } = *self.heap.get(v.heap_index())
        {
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
    pub(crate) fn canonicalize_locale_list(
        &mut self,
        locales: Value,
    ) -> Result<Vec<String>, Thrown> {
        let mut out: Vec<String> = vec![];
        if locales == Value::UNDEFINED {
            return Ok(out);
        }
        if let Some(tag) = self.locale_slot_tag(locales) {
            out.push(tag);
            return Ok(out);
        }
        if locales.is_heap() && self.heap.is_str_like(locales.heap_index()) {
            let s = self
                .heap
                .str_cow(locales.heap_index())
                .unwrap()
                .into_owned();
            self.preflight_native_iteration_work(crate::vm::locale_tag::locale_parse_work_bound(
                s.len(),
            ))?;
            out.push(canonicalize_locale(&s).ok_or_else(|| {
                Thrown(format!(
                    "RangeError: Incorrect locale information provided: {s}"
                ))
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
        let len = if len_f.is_nan() || len_f <= 0.0 {
            0u64
        } else {
            len_f.min(9.007e15) as u64
        };
        // The loop below is one native operation: holes, proxy traps, locale
        // parsing, and deduplication do not return to the bytecode step meter.
        // Reject the full ToLength before observing any indexed property.
        self.preflight_native_iteration_work(len)?;

        // CanonicalizeLocaleList preserves first-seen order, so `out` remains a
        // Vec. Keep a randomized hash set beside it to avoid the former O(n^2)
        // Vec::contains scan for many distinct attacker-selected tags. Bound
        // the aggregate text scanned and retained as well as the element count:
        // 262k individually near-limit locale strings must not become hundreds
        // of gigabytes of invisible native work.
        let mut seen: HashSet<String> = HashSet::new();
        let mut processed_bytes = 0usize;
        let mut retained_bytes = 0usize;
        let mut parse_work = 0u64;
        for i in 0..len {
            let key = Value::num(i as f64);
            if !self.has_property_dyn(obj, key)? {
                continue;
            }
            let el = self.get_index(obj, key)?;
            if let Some(tag) = self.locale_slot_tag(el) {
                account_locale_list_bytes(&mut processed_bytes, tag.len())?;
                parse_work = parse_work
                    .saturating_add(crate::vm::locale_tag::locale_parse_work_bound(tag.len()));
                self.preflight_native_iteration_work(parse_work)?;
                push_unique_locale(&mut out, &mut seen, &mut retained_bytes, tag)?;
                continue;
            }
            let is_string = el.is_heap() && self.heap.is_str_like(el.heap_index());
            if !is_string && !self.is_object_value(el) {
                return Err(Thrown(
                    "TypeError: locale list elements must be strings or objects".into(),
                ));
            }
            let s = self.to_js_string(el)?;
            account_locale_list_bytes(&mut processed_bytes, s.len())?;
            parse_work =
                parse_work.saturating_add(crate::vm::locale_tag::locale_parse_work_bound(s.len()));
            self.preflight_native_iteration_work(parse_work)?;
            let c = canonicalize_locale(&s).ok_or_else(|| {
                Thrown(format!(
                    "RangeError: Incorrect locale information provided: {s}"
                ))
            })?;
            push_unique_locale(&mut out, &mut seen, &mut retained_bytes, c)?;
        }
        Ok(out)
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
        let s = self.to_js_string(v)?;
        self.preflight_native_iteration_work(s.len() as u64)?;
        Ok(s)
    }

    /// RangeError if `s` is not in `allowed` (when non-empty) — the deferred
    /// validation half of `opt_string_raw`.
    pub(crate) fn unit_allowed(&self, s: &str, key: &str, allowed: &[&str]) -> Result<(), Thrown> {
        if !allowed.is_empty() && !allowed.contains(&s) {
            return Err(Thrown(format!(
                "RangeError: Value {s} out of range for option {key}"
            )));
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
        self.preflight_native_iteration_work(s.len() as u64)?;
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
        self.preflight_native_iteration_work(s.len() as u64)?;
        self.unit_allowed(&s, key, allowed)?;
        Ok(Some(s))
    }

    /// GetOption(options, key, boolean, empty, undefined) — ToBoolean of a present
    /// value, `None` when absent (DateTimeFormat's `hour12` distinguishes the two:
    /// only a PRESENT hour12 overrides hourCycle).
    pub(crate) fn opt_bool_opt(
        &mut self,
        options: Value,
        key: &str,
    ) -> Result<Option<bool>, Thrown> {
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
        let (su_string, options) = if arg.is_heap() && self.heap.is_str_like(arg.heap_index()) {
            (Some(arg), Value::UNDEFINED)
        } else if arg == Value::UNDEFINED {
            return Err(Thrown(
                "TypeError: round() requires an options argument".into(),
            ));
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
        let smallest = self.to_js_string(smallest_v)?;
        self.preflight_native_iteration_work(smallest.len() as u64)?;
        let su = normalize_unit(&smallest, "");
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
        let units = [
            "hour",
            "minute",
            "second",
            "millisecond",
            "microsecond",
            "nanosecond",
        ];
        let rank = |u: &str| units.iter().position(|&x| x == u).unwrap_or(5) as i32;
        if options == Value::UNDEFINED {
            return Ok((
                default_largest.to_string(),
                "nanosecond".to_string(),
                1,
                "trunc".to_string(),
            ));
        }
        if !self.is_object_value(options) {
            return Err(Thrown(
                "TypeError: options must be an object or undefined".into(),
            ));
        }
        let largest_allowed = [
            "auto",
            "hour",
            "hours",
            "minute",
            "minutes",
            "second",
            "seconds",
            "millisecond",
            "milliseconds",
            "microsecond",
            "microseconds",
            "nanosecond",
            "nanoseconds",
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
            if rank(default_largest) <= rank(&su) {
                default_largest.to_string()
            } else {
                su.clone()
            }
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
                    return Err(Thrown(
                        "TypeError: Locale tag must be a string or an object".into(),
                    ));
                }
                self.to_js_string(tag)?
            }
        };
        self.preflight_native_iteration_work(lt::locale_parse_work_bound(base.len()))?;
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
            let base_work = lt::locale_parse_work_bound(v.len());
            self.preflight_native_iteration_work(base_work)?;
            let mut seen: Vec<String> = vec![];
            let mut membership: HashSet<String> = HashSet::new();
            for part in v.split('-') {
                let lp = part.to_ascii_lowercase();
                if !lt::is_variant_subtag(&lp) || membership.contains(&lp) {
                    return Err(Thrown(format!("RangeError: invalid variants option: {v}")));
                }
                seen.try_reserve(1)
                    .map_err(|_| Thrown("RangeError: locale variants allocation failed".into()))?;
                membership
                    .try_reserve(1)
                    .map_err(|_| Thrown("RangeError: locale variants allocation failed".into()))?;
                membership.insert(lp.clone());
                seen.push(lp);
            }
            let total_work = base_work.saturating_add(lt::locale_sort_work_bound(seen.len()));
            self.preflight_native_iteration_work(total_work)?;
            seen.sort();
            t.variants = seen;
        }
        // ApplyOptionsToTag step 13 — a region supplied as an option is itself
        // subject to the registries (`{region: "554"}` on "en" is "en-NZ").
        crate::vm::cldr_alias::canonicalize(&mut t);
        // ── ApplyUnicodeExtensionToTag: the relevant extension keys, in key order ──
        for (opt_name, key) in [
            ("calendar", "ca"),
            ("collation", "co"),
            ("firstDayOfWeek", "fw"),
            ("hourCycle", "hc"),
        ] {
            let raw = if key == "fw" {
                // firstDayOfWeek also accepts the numeric weekday spellings
                // (and `true`, which ToString's to "true" → the bare `-u-fw-`).
                self.opt_string_opt(options, opt_name, &[])?
                    .map(|s| lt::weekday_to_string(&s))
            } else {
                self.opt_string_opt(options, opt_name, &[])?
            };
            if let Some(v) = raw {
                if key == "hc" && !["h11", "h12", "h23", "h24"].contains(&v.as_str()) {
                    return Err(Thrown(format!("RangeError: invalid hourCycle option: {v}")));
                }
                if key != "hc" && !lt::is_type_sequence(&v) {
                    return Err(Thrown(format!(
                        "RangeError: invalid {opt_name} option: {v}"
                    )));
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
            t.set_u(
                "kn",
                Some(if n {
                    String::new()
                } else {
                    "false".to_string()
                }),
            );
        }
        if let Some(ns) = self.opt_string_opt(options, "numberingSystem", &[])? {
            if !lt::is_type_sequence(&ns) {
                return Err(Thrown(format!(
                    "RangeError: invalid numberingSystem option: {ns}"
                )));
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
        for (key, val) in [("script", t.script.clone()), ("region", t.region.clone())] {
            let v = if val.is_empty() {
                Value::UNDEFINED
            } else {
                self.alloc_str(val)
            };
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
        r.set(
            "@@fwindex",
            match fw_idx {
                Some(i) => Value::num(i as f64),
                None => Value::UNDEFINED,
            },
        );
        let resolved = self.heap.alloc(HeapObj::Object(Box::new(r)));
        let idx = self.heap.alloc(HeapObj::Intl {
            kind: native::INTL_LOCALE,
            resolved,
        });
        if self.intl_protos[native::INTL_LOCALE as usize] != 0 {
            self.proto_of.insert(
                idx,
                Value::heap(self.intl_protos[native::INTL_LOCALE as usize]),
            );
        }
        Value::heap(idx)
    }

    /// StringListFromIterable (ECMA-402 13.5.1) — Intl.ListFormat's argument
    /// coercion. `undefined` is an empty list (NOT a TypeError), and a yielded
    /// value that is not a String stops iteration at once with a TypeError,
    /// after IteratorClose. Draining first and checking after would run the
    /// iterator past the offending element, which `iterable-iteratorclose`
    /// observes through the iterator's own step counter.
    pub(crate) fn string_list_from_iterable(
        &mut self,
        iterable: Value,
    ) -> Result<Vec<String>, Thrown> {
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
            self.preflight_native_iteration_work(vals.len() as u64)?;
            let mut out = Vec::new();
            out.try_reserve_exact(vals.len())
                .map_err(|_| Thrown("RangeError: list allocation failed".into()))?;
            let mut total_bytes = 0usize;
            for v in vals {
                self.push_string_list_value(&mut out, &mut total_bytes, v)?;
            }
            return Ok(out);
        };
        let mut out: Vec<String> = vec![];
        let mut total_bytes = 0usize;
        let mut iterations = 0u64;
        loop {
            iterations = iterations.saturating_add(1);
            self.preflight_native_iteration_work(iterations)?;
            let res = self.call_value(next, iter, &[])?;
            if !self.is_object_value(res) {
                return Err(Thrown(
                    "TypeError: iterator.next() returned a non-object".into(),
                ));
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
            if let Err(error) = self.push_string_list_value(&mut out, &mut total_bytes, v) {
                self.iterator_close_quiet(iter);
                return Err(error);
            }
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
                let pref = self
                    .display(self.intl_slot(resolved, "calendar"))
                    .to_ascii_lowercase();
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
                    self.heap
                        .alloc(HeapObj::Array(vec![Value::num(6.0), Value::num(7.0)])),
                );
                o.set("weekend", weekend);
                let _ = tag;
                Value::heap(self.heap.alloc(HeapObj::Object(Box::new(o))))
            }
        })
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
    // BCP-47 carries deprecated aliases for two calendars; ECMA-402 works in the
    // CANONICAL id, so `{calendar: "ethiopic-amete-alem"}` and `-u-ca-islamicc`
    // must resolve exactly as `ethioaa` and `islamic-civil` do rather than being
    // rejected as unsupported and falling back to the default
    // (`canonicalize-calendar.js`). Applied before the availability filter, so
    // the alias is measured against the same list as its canonical form.
    let canon = |s: String| -> String {
        match s.as_str() {
            "ethiopic-amete-alem" => "ethioaa".to_string(),
            "islamicc" => "islamic-civil".to_string(),
            _ => s,
        }
    };
    let ok = |v: &String| available.is_empty() || available.contains(&v.as_str());
    let opt = option.map(|s| canon(s.to_ascii_lowercase())).filter(&ok);
    let ext_ok = ext.map(|s| canon(s.to_ascii_lowercase())).filter(&ok);
    let value = opt
        .or_else(|| ext_ok.clone())
        .unwrap_or_else(|| default.to_string());
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

/// A `-u-` keyword value as it is stored: lowercased, with the redundant
/// explicit "true" dropped (`-u-kn-true` canonicalizes to `-u-kn`).
fn canon_ext_value(v: &str) -> String {
    let l = v.to_ascii_lowercase();
    if l == "true" {
        String::new()
    } else {
        l
    }
}

/// The ISO 15924 script codes written right-to-left. This is a Unicode fact
/// about the scripts themselves (their characters' Bidi_Class), not per-locale
/// CLDR data, so `Intl.Locale.prototype.getTextInfo` can answer it from the
/// script subtag alone.
fn is_rtl_script(script: &str) -> bool {
    matches!(
        script,
        "Adlm"
            | "Arab"
            | "Aran"
            | "Armi"
            | "Avst"
            | "Cprt"
            | "Egyp"
            | "Elym"
            | "Gara"
            | "Hatr"
            | "Hebr"
            | "Hung"
            | "Khar"
            | "Lydi"
            | "Mand"
            | "Mani"
            | "Mend"
            | "Merc"
            | "Mero"
            | "Narb"
            | "Nbat"
            | "Nkoo"
            | "Orkh"
            | "Palm"
            | "Phli"
            | "Phlp"
            | "Phnx"
            | "Prti"
            | "Rohg"
            | "Samr"
            | "Sarb"
            | "Sogd"
            | "Sogo"
            | "Syrc"
            | "Thaa"
            | "Todr"
            | "Yezi"
    )
}

/// Whether a canonical tag carries a `-u-<key>` keyword at all, with or without
/// a value — `-u-kn` (the canonical form of `-u-kn-true`) has no value subtag,
/// so `unicode_ext_value` alone cannot distinguish it from an absent key.
/// `[[AvailableLocales]]` — the locales this engine actually has content for.
///
/// It is `["en", "en-US"]` because `vm/cldr_en.rs` is the whole of the locale
/// data zipp ships (CLDR 47 `en`, V8's `small-icu` shape). This list must never
/// grow past what is bundled: an engine that answers `supportedLocalesOf(["de"])`
/// with `["de"]` and then formats German with English month names is lying about
/// what it can do, and `Intl.NumberFormat("de").resolvedOptions().locale === "de"`
/// is the same lie in the other direction.
///
/// Sorted, and every entry is a canonical tag — `best_available_locale` relies
/// on both.
pub(crate) const AVAILABLE_LOCALES: &[&str] = &["en", "en-US"];

/// DefaultLocale(). Must itself be in [[AvailableLocales]].
pub(crate) const DEFAULT_LOCALE: &str = "en";

/// A tag with its `-u-`, `-t-` and `-x-` extension sequences removed — the
/// "noExtensionsLocale" every matcher works on.
pub(crate) fn strip_extensions(tag: &str) -> String {
    let mut out: Vec<&str> = vec![];
    let mut skipping = false;
    for (i, sub) in tag.split('-').enumerate() {
        // A singleton subtag (one alphanumeric) opens an extension sequence that
        // runs to the next singleton or the end. Position 0 can never be one.
        if i > 0 && sub.len() == 1 {
            skipping = true;
            continue;
        }
        if !skipping {
            out.push(sub);
        }
    }
    out.join("-")
}

/// The `-u-` extension subtags of a tag ("ca-buddhist-nu-arab"), or `None`.
pub(crate) fn unicode_extension_of(tag: &str) -> Option<String> {
    let rest = tag.split("-u-").nth(1)?;
    // The `-u-` sequence ends at the next singleton subtag (`-t-`, `-x-`, …).
    let mut out: Vec<&str> = vec![];
    for sub in rest.split('-') {
        if sub.len() == 1 {
            break;
        }
        out.push(sub);
    }
    (!out.is_empty()).then(|| out.join("-"))
}

/// ResolveLocale's LookupMatcher (ECMA-402 9.2.3): the first requested tag
/// whose no-extension form has a BestAvailableLocale match, carrying that tag's
/// `-u-` extension forward so the callers' `unicode_ext_value` still sees the
/// requested keywords.
///
/// No match anywhere falls back to DefaultLocale() and — per LookupMatcher
/// step 5 — WITHOUT an extension: the keywords of a locale the engine does not
/// have say nothing about the one it will actually use. That is why
/// `new Intl.DateTimeFormat("de-u-hc-h11")` resolves to plain `en` here rather
/// than to `en-u-hc-h11` (`resolvedOptions/hourCycle.js` wants the h11, and
/// only a `de` in [[AvailableLocales]] would earn it).
pub(crate) fn lookup_matcher(requested: &[String]) -> String {
    requested
        .iter()
        .find_map(|tag| {
            best_available_locale(&strip_extensions(tag)).map(|found| {
                // Only the `-u-` travels; `-t-`/`-x-` carry no
                // [[RelevantExtensionKeys]].
                match unicode_extension_of(tag) {
                    Some(ext) => format!("{found}-u-{ext}"),
                    None => found,
                }
            })
        })
        .unwrap_or_else(|| DEFAULT_LOCALE.to_string())
}

/// BestAvailableLocale (ECMA-402 9.2.2): the longest prefix of `locale` that is
/// in [[AvailableLocales]], truncating one subtag at a time and never leaving a
/// trailing single-character subtag behind. `locale` must already have its
/// extensions stripped.
pub(crate) fn best_available_locale(locale: &str) -> Option<String> {
    let mut candidate = locale.to_string();
    loop {
        if AVAILABLE_LOCALES
            .iter()
            .any(|a| a.eq_ignore_ascii_case(&candidate))
        {
            return Some(candidate);
        }
        let Some(pos) = candidate.rfind('-') else {
            return None;
        };
        // Step 2c: if the truncation would leave a one-character subtag
        // dangling ("en-a-bbb" → "en-a"), drop that too.
        let cut = if pos >= 2 && candidate.as_bytes()[pos - 2] == b'-' {
            pos - 2
        } else {
            pos
        };
        candidate.truncate(cut);
        if candidate.is_empty() {
            return None;
        }
    }
}

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
    if out.is_empty() {
        None
    } else {
        Some(out.join("-"))
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
    crate::vm::temporal::tzdb::primary_ids()
        .into_iter()
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod locale_matcher_tests {
    use super::*;

    #[test]
    fn best_available_truncates_subtag_by_subtag() {
        assert_eq!(best_available_locale("en").as_deref(), Some("en"));
        assert_eq!(best_available_locale("en-US").as_deref(), Some("en-US"));
        // Not bundled, so it falls back to the parent that is.
        assert_eq!(best_available_locale("en-GB").as_deref(), Some("en"));
        assert_eq!(best_available_locale("en-Latn-GB").as_deref(), Some("en"));
        assert_eq!(best_available_locale("de").as_deref(), None);
        assert_eq!(best_available_locale("zxx").as_deref(), None);
    }

    #[test]
    fn extensions_are_split_off_and_re_attached() {
        assert_eq!(strip_extensions("en-US-u-ca-gregory-nu-latn"), "en-US");
        assert_eq!(strip_extensions("en-u-hc-h11-x-priv"), "en");
        assert_eq!(strip_extensions("en-Latn-US"), "en-Latn-US");
        assert_eq!(
            unicode_extension_of("en-u-ca-gregory-x-p").as_deref(),
            Some("ca-gregory")
        );
        assert_eq!(unicode_extension_of("en-US").as_deref(), None);
    }

    #[test]
    fn lookup_matcher_keeps_the_extension_only_on_a_real_match() {
        assert_eq!(lookup_matcher(&["en-u-hc-h11".into()]), "en-u-hc-h11");
        assert_eq!(lookup_matcher(&["en-GB-u-hc-h11".into()]), "en-u-hc-h11");
        // LookupMatcher step 5: the default locale, and the extension is gone.
        assert_eq!(lookup_matcher(&["de-u-hc-h11".into()]), "en");
        // The FIRST tag that MATCHES wins, not simply the first tag.
        assert_eq!(lookup_matcher(&["de".into(), "en-US".into()]), "en-US");
        assert_eq!(lookup_matcher(&[]), "en");
    }
}
