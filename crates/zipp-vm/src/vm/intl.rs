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
        Value::heap(self.heap.alloc(HeapObj::Object(o)))
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

    /// Read a string option (returns `default` if undefined); validates against
    /// `allowed` when non-empty (→ RangeError).
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
        if !allowed.is_empty() && !allowed.contains(&s.as_str()) {
            return Err(Thrown(format!("RangeError: Value {s} out of range for option {key}")));
        }
        Ok(s)
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
        let lu = normalize_unit(
            &self.opt_string(options, "largestUnit", "auto", &largest_allowed)?,
            default_largest,
        );
        let su = normalize_unit(
            &self.opt_string(options, "smallestUnit", "nanosecond", small_allowed)?,
            "nanosecond",
        );
        let inc = self.read_rounding_increment(options)?;
        let mode = self.read_rounding_mode(options, "trunc")?;
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

    /// `new Intl.<service>(locales, options)` → build resolved options + instance.
    pub(crate) fn make_intl(&mut self, kind: u8, locales: Value, options: Value) -> Result<Value, Thrown> {
        use native::*;
        // options must be undefined or coercible to object (per spec GetOptionsObject).
        if options != Value::UNDEFINED && !self.is_object_value(options) {
            // Collator/NumberFormat/DateTimeFormat use ToObject; Locale's 2nd arg too.
            // A primitive options arg is a TypeError for these services.
            return Err(Thrown("TypeError: Options must be an object or undefined".into()));
        }
        if kind == INTL_LOCALE {
            return self.make_locale(locales, options);
        }
        let locale = self.resolve_locale(locales)?;
        let loc = self.alloc_str(locale.clone());
        let mut r = ObjMap::new();
        r.set("locale", loc);
        match kind {
            INTL_NUMBERFORMAT => {
                let ns = self.alloc_str("latn".to_string());
                r.set("numberingSystem", ns);
                let style = self.opt_string(
                    options,
                    "style",
                    "decimal",
                    &["decimal", "percent", "currency", "unit"],
                )?;
                let sv = self.alloc_str(style.clone());
                r.set("style", sv);
                if style == "currency" {
                    let cur = self.opt_string(options, "currency", "", &[])?;
                    if cur.is_empty() {
                        return Err(Thrown(
                            "TypeError: currency must be provided for style 'currency'".into(),
                        ));
                    }
                    let cv = self.alloc_str(cur.to_uppercase());
                    r.set("currency", cv);
                    let cd = self.opt_string(
                        options,
                        "currencyDisplay",
                        "symbol",
                        &["symbol", "narrowSymbol", "code", "name"],
                    )?;
                    let cdv = self.alloc_str(cd);
                    r.set("currencyDisplay", cdv);
                }
                let min_int = self.opt_int(options, "minimumIntegerDigits", 1, 1, 21)?;
                let def_min_frac = if style == "currency" { 2 } else { 0 };
                let def_max_frac = if style == "currency" {
                    2
                } else if style == "percent" {
                    0
                } else {
                    3
                };
                let min_frac =
                    self.opt_int(options, "minimumFractionDigits", def_min_frac, 0, 100)?;
                let max_frac = self
                    .opt_int(options, "maximumFractionDigits", def_max_frac.max(min_frac), 0, 100)?;
                r.set("minimumIntegerDigits", Value::num(min_int as f64));
                r.set("minimumFractionDigits", Value::num(min_frac as f64));
                r.set("maximumFractionDigits", Value::num(max_frac as f64));
                let ug = self.opt_string(
                    options,
                    "useGrouping",
                    "auto",
                    &["auto", "always", "min2", "true", "false"],
                )?;
                // ES2023 useGrouping is a string|boolean; echo "auto"/"always" as given.
                let ugv = self.alloc_str(ug);
                r.set("useGrouping", ugv);
                let notation = self.opt_string(
                    options,
                    "notation",
                    "standard",
                    &["standard", "scientific", "engineering", "compact"],
                )?;
                let nv = self.alloc_str(notation);
                r.set("notation", nv);
                let sd = self.opt_string(
                    options,
                    "signDisplay",
                    "auto",
                    &["auto", "never", "always", "exceptZero", "negative"],
                )?;
                let sdv = self.alloc_str(sd);
                r.set("signDisplay", sdv);
            }
            INTL_DATETIMEFORMAT => {
                let ns = self.alloc_str("latn".to_string());
                r.set("numberingSystem", ns);
                let cal = self.alloc_str("gregory".to_string());
                r.set("calendar", cal);
                let tz = self.opt_string(options, "timeZone", "UTC", &[])?;
                let tzv = self.alloc_str(tz);
                r.set("timeZone", tzv);
                // If no explicit date/time components and no dateStyle/timeStyle,
                // default to year/month/day.
                let comps = [
                    ("weekday", &["narrow", "short", "long"][..]),
                    ("era", &["narrow", "short", "long"][..]),
                    ("year", &["2-digit", "numeric"][..]),
                    ("month", &["2-digit", "numeric", "narrow", "short", "long"][..]),
                    ("day", &["2-digit", "numeric"][..]),
                    ("hour", &["2-digit", "numeric"][..]),
                    ("minute", &["2-digit", "numeric"][..]),
                    ("second", &["2-digit", "numeric"][..]),
                ];
                let mut any = false;
                let mut vals: Vec<(&str, String)> = vec![];
                for (name, allowed) in comps {
                    let v = self.opt_string(options, name, "", allowed)?;
                    if !v.is_empty() {
                        any = true;
                        vals.push((name, v));
                    }
                }
                let date_style = self.opt_string(
                    options,
                    "dateStyle",
                    "",
                    &["full", "long", "medium", "short"],
                )?;
                let time_style = self.opt_string(
                    options,
                    "timeStyle",
                    "",
                    &["full", "long", "medium", "short"],
                )?;
                if !date_style.is_empty() {
                    let v = self.alloc_str(date_style);
                    r.set("dateStyle", v);
                }
                if !time_style.is_empty() {
                    let v = self.alloc_str(time_style);
                    r.set("timeStyle", v);
                }
                if !any && r.pos("dateStyle").is_none() && r.pos("timeStyle").is_none() {
                    vals = vec![
                        ("year", "numeric".to_string()),
                        ("month", "numeric".to_string()),
                        ("day", "numeric".to_string()),
                    ];
                }
                for (name, v) in vals {
                    let vv = self.alloc_str(v);
                    r.set(name, vv);
                }
                let hc = self.opt_string(options, "hour12", "", &[])?;
                if !hc.is_empty() && r.pos("hour").is_some() {
                    // (left implicit; hourCycle handling is minimal)
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
                let t = self.opt_string(options, "type", "cardinal", &["cardinal", "ordinal"])?;
                let tv = self.alloc_str(t);
                r.set("type", tv);
                let min_int = self.opt_int(options, "minimumIntegerDigits", 1, 1, 21)?;
                let min_frac = self.opt_int(options, "minimumFractionDigits", 0, 0, 100)?;
                let max_frac = self.opt_int(options, "maximumFractionDigits", 3.max(min_frac), 0, 100)?;
                r.set("minimumIntegerDigits", Value::num(min_int as f64));
                r.set("minimumFractionDigits", Value::num(min_frac as f64));
                r.set("maximumFractionDigits", Value::num(max_frac as f64));
                let ns = self.alloc_str("latn".to_string());
                r.set("pluralCategories", Value::UNDEFINED); // filled below as array
                r.set("numberingSystem", ns);
                let cats = ["one", "other"]
                    .iter()
                    .map(|c| self.alloc_str(c.to_string()))
                    .collect::<Vec<_>>();
                let arr = Value::heap(self.heap.alloc(HeapObj::Array(cats)));
                r.set("pluralCategories", arr);
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
        let resolved = self.heap.alloc(HeapObj::Object(r));
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
        let resolved = self.heap.alloc(HeapObj::Object(r));
        let idx = self.heap.alloc(HeapObj::Intl { kind: native::INTL_LOCALE, resolved });
        if self.intl_protos[native::INTL_LOCALE as usize] != 0 {
            self.proto_of.insert(idx, Value::heap(self.intl_protos[native::INTL_LOCALE as usize]));
        }
        Ok(Value::heap(idx))
    }

    /// Intl.NumberFormat.prototype.format(value).
    pub(crate) fn intl_number_format(&mut self, resolved: u32, value: Value) -> Result<Value, Thrown> {
        let n = self.to_number(value)?;
        let style = self.display(self.intl_slot(resolved, "style"));
        let min_frac = self.to_number(self.intl_slot(resolved, "minimumFractionDigits"))? as i64;
        let max_frac = self.to_number(self.intl_slot(resolved, "maximumFractionDigits"))? as i64;
        let min_int = self.to_number(self.intl_slot(resolved, "minimumIntegerDigits"))? as i64;
        let ug = self.display(self.intl_slot(resolved, "useGrouping"));
        let grouping = ug != "false";
        let s = format_number_intl(n, &style, min_frac, max_frac, min_int, grouping);
        let out = if style == "percent" {
            s
        } else if style == "currency" {
            let cur = self.display(self.intl_slot(resolved, "currency"));
            format!("{}{}", currency_symbol(&cur), s)
        } else {
            s
        };
        Ok(self.alloc_str(out))
    }

    /// Intl.DateTimeFormat.prototype.format(date) — UTC, en-US conventions.
    pub(crate) fn dtf_format(&self, resolved: u32, ms: f64) -> String {
        let total_ms = ms as i128;
        let days = total_ms.div_euclid(86_400_000) as i64;
        let (y, mo, d) = epoch_days_to_iso(days);
        let rem_ns = total_ms.rem_euclid(86_400_000) * 1_000_000;
        let t = ns_to_time(rem_ns); // [h, mi, s, ms, us, ns]
        let has = |k: &str| matches!(self.heap.get(resolved), HeapObj::Object(m) if m.pos(k).is_some());
        let has_date = has("year") || has("month") || has("day") || has("dateStyle") || has("weekday");
        let has_time = has("hour") || has("minute") || has("second") || has("timeStyle");
        let date_part = format!("{}/{}/{}", mo, d, y);
        let mut out = String::new();
        if has_date {
            out.push_str(&date_part);
        }
        if has_time {
            let h24 = t[0];
            let (h12, ap) = if h24 == 0 {
                (12, "AM")
            } else if h24 < 12 {
                (h24, "AM")
            } else if h24 == 12 {
                (12, "PM")
            } else {
                (h24 - 12, "PM")
            };
            let ts = format!("{}:{:02}:{:02}\u{202f}{}", h12, t[1], t[2], ap);
            if has_date {
                out.push_str(", ");
            }
            out.push_str(&ts);
        }
        if out.is_empty() {
            out = date_part;
        }
        out
    }

}
