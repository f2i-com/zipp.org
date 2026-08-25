#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;
use crate::vm::{cldr_en, dtf_pattern};

pub(crate) mod collator;
pub(crate) mod datetime_format;
pub(crate) mod datetime_options;
pub(crate) mod display_names;
pub(crate) mod duration;
pub(crate) mod list;
pub(crate) mod number;
pub(crate) mod relative_time;
pub(crate) mod segment;
pub(crate) mod shared;

pub(crate) use collator::*;
pub(crate) use datetime_options::*;
pub(crate) use display_names::*;
pub(crate) use duration::*;
pub(crate) use number::*;
pub(crate) use segment::*;
pub(crate) use shared::*;

impl<'p> Vm<'p> {
    /// `new Intl.<service>(locales, options)` → build resolved options + instance.
    pub(crate) fn make_intl(
        &mut self,
        kind: u8,
        locales: Value,
        options: Value,
    ) -> Result<Value, Thrown> {
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
                return Err(Thrown(
                    "TypeError: Options must be an object or undefined".into(),
                ));
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
                    return Err(Thrown(
                        "TypeError: Options must be an object or undefined".into(),
                    ));
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
        self.opt_string(
            options,
            "localeMatcher",
            "best fit",
            &["lookup", "best fit"],
        )?;
        let locale = lookup_matcher(&requested);
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
                let currency_sign = self.opt_string(
                    options,
                    "currencySign",
                    "standard",
                    &["standard", "accounting"],
                )?;
                let unit = self.opt_string_opt(options, "unit", &[])?;
                if let Some(ref u) = unit {
                    if !is_well_formed_unit(u) {
                        return Err(Thrown(format!("RangeError: invalid unit identifier: {u}")));
                    }
                }
                if style == "unit" && unit.is_none() {
                    return Err(Thrown(
                        "TypeError: unit must be provided for style 'unit'".into(),
                    ));
                }
                let unit_display = self.opt_string(
                    options,
                    "unitDisplay",
                    "short",
                    &["short", "narrow", "long"],
                )?;
                let notation = self.opt_string(
                    options,
                    "notation",
                    "standard",
                    &["standard", "scientific", "engineering", "compact"],
                )?;
                // The fraction-digit defaults depend on the style: a currency uses
                // its minor-unit count, a percent 0, anything else 0..3.
                // InitializeNumberFormat step 19 gates the currency case on
                // `notation is "standard"` — under scientific/engineering the
                // minor-unit count is NOT the default, so KWD reported 3..3 where
                // the spec's step 20 says 0..3
                // (NumberFormat/currency-digits-nonstandard-notation.js).
                // `compact` reaches SetNumberFormatDigitOptions' morePrecision
                // branch, which overrides both regardless.
                let (mnfd_def, mxfd_def) = match style.as_str() {
                    "currency" if notation == "standard" => {
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
                let default_grouping = if notation == "compact" {
                    "min2"
                } else {
                    "auto"
                };
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
                r.set(
                    "roundingIncrement",
                    Value::num(digits.rounding_increment as f64),
                );
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
                let hour_cycle_opt = if hour12.is_some() {
                    None
                } else {
                    hour_cycle_opt
                };
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
                        None => return Err(Thrown(format!("RangeError: invalid time zone: {s}"))),
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
                    (
                        "timeZoneName",
                        &[
                            "short",
                            "long",
                            "shortOffset",
                            "longOffset",
                            "shortGeneric",
                            "longGeneric",
                        ],
                    ),
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
                let _ =
                    self.opt_string(options, "formatMatcher", "best fit", &["basic", "best fit"])?;
                let date_style = self.opt_string_opt(
                    options,
                    "dateStyle",
                    &["full", "long", "medium", "short"],
                )?;
                let time_style = self.opt_string_opt(
                    options,
                    "timeStyle",
                    &["full", "long", "medium", "short"],
                )?;
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
                        for (name, v) in [
                            ("year", "numeric"),
                            ("month", "numeric"),
                            ("day", "numeric"),
                        ] {
                            vals.push((name, v.to_string()));
                        }
                    }
                    if def_time {
                        for (name, v) in [
                            ("hour", "numeric"),
                            ("minute", "numeric"),
                            ("second", "numeric"),
                        ] {
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
                        comps
                            .iter()
                            .position(|(c, _)| *c == n)
                            .unwrap_or(usize::MAX)
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
                // An explicit `hour12` overrides both the `hourCycle` option
                // and `-u-hc-`, and (ECMA-402) drops the keyword from the
                // resolved locale; otherwise this is an ordinary
                // option-then-extension resolution.
                //
                // ResolveLocale runs over ["ca","nu","hc"] BEFORE the component
                // options are read, so a `-u-hc-` the request carried stays in
                // [[Locale]] whether or not the resolved pattern ends up with an
                // hour field. Only the `hourCycle`/`hour12` entries of
                // resolvedOptions are gated on the hour
                // (`resolvedOptions/resolved-locale-with-hc-unicode.js` asserts
                // exactly that split, "Without hour option").
                let hc = match (hour12, hour_cycle_opt.clone()) {
                    // hour12:false is h23; hour12:true keeps the locale's
                    // 12-hour cycle (h12 for the en-style default).
                    (Some(false), _) => "h23".to_string(),
                    (Some(true), _) => "h12".to_string(),
                    (None, opt) => {
                        let (v, keep) =
                            resolve_ext_key(opt, ext("hc"), &["h11", "h12", "h23", "h24"], "h12");
                        if keep {
                            ext_used.push(("hc", v.clone()));
                        }
                        v
                    }
                };
                if has_hour {
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
                r.set(
                    "ignorePunctuation",
                    Value::bool(ignore_punct.unwrap_or(false)),
                );
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
                    ext_used.push((
                        "kn",
                        if kn_s == "true" {
                            String::new()
                        } else {
                            kn_s.clone()
                        },
                    ));
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
                r.set(
                    "roundingIncrement",
                    Value::num(digits.rounding_increment as f64),
                );
                r.set("roundingMode", rm);
                let rp = self.alloc_str(digits.rounding_priority.clone());
                r.set("roundingPriority", rp);
                let tzd = self.alloc_str(digits.trailing_zero_display.clone());
                r.set("trailingZeroDisplay", tzd);
            }
            INTL_LISTFORMAT => {
                let t = self.opt_string(
                    options,
                    "type",
                    "conjunction",
                    &["conjunction", "disjunction", "unit"],
                )?;
                let tv = self.alloc_str(t);
                r.set("type", tv);
                let st = self.opt_string(options, "style", "long", &["long", "short", "narrow"])?;
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
                let g = self.opt_string(
                    options,
                    "granularity",
                    "grapheme",
                    &["grapheme", "word", "sentence"],
                )?;
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
                    &[
                        "language",
                        "region",
                        "script",
                        "currency",
                        "calendar",
                        "dateTimeField",
                    ],
                )?;
                let t = t.ok_or_else(|| {
                    Thrown("TypeError: Intl.DisplayNames type option is required".into())
                })?;
                let fb = self.opt_string(options, "fallback", "code", &["code", "none"])?;
                let ld = self.opt_string(
                    options,
                    "languageDisplay",
                    "dialect",
                    &["dialect", "standard"],
                )?;
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
                let st = self.opt_string(
                    options,
                    "style",
                    "short",
                    &["long", "short", "narrow", "digital"],
                )?;
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
                            if numeric_ok {
                                "numeric".to_string()
                            } else {
                                "short".to_string()
                            }
                        }
                        None if matches!(
                            prev_style.as_str(),
                            "fractional" | "numeric" | "2-digit"
                        ) =>
                        {
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
                            if st == "long" || st == "narrow" {
                                st.clone()
                            } else {
                                "short".to_string()
                            }
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
            self.proto_of
                .insert(idx, Value::heap(self.intl_protos[kind as usize]));
        }
        Ok(Value::heap(idx))
    }
}
