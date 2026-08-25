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

impl<'p> Vm<'p> {
    /// SetNumberFormatDigitOptions (ECMA-402 15.1.3) — shared verbatim by
    /// Intl.NumberFormat and Intl.PluralRules. Reads nine options in one fixed
    /// order (the `constructor-option-read-order` tests observe every Get), then
    /// resolves which of the fraction-digit / significant-digit pairs actually
    /// apply. Returns the resolved slots; the caller decides where they land in
    /// its own resolvedOptions table.
    pub(crate) fn read_number_format_digit_options(
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
            if options == Value::UNDEFINED {
                Ok(Value::UNDEFINED)
            } else {
                vm.get_prop(options, k)
            }
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
                "ceil",
                "floor",
                "expand",
                "trunc",
                "halfCeil",
                "halfFloor",
                "halfExpand",
                "halfTrunc",
                "halfEven",
            ],
        )?;
        let rounding_priority = self.opt_string(
            options,
            "roundingPriority",
            "auto",
            &["auto", "morePrecision", "lessPrecision"],
        )?;
        let trailing_zero_display = self.opt_string(
            options,
            "trailingZeroDisplay",
            "auto",
            &["auto", "stripIfInteger"],
        )?;
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
                let mnsd = self
                    .default_number_option(mnsd_raw, "minimumSignificantDigits", 1, 21)?
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
    pub(crate) fn store_digit_options(&mut self, r: &mut ObjMap, d: &DigitOptions) {
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
    pub(crate) fn read_use_grouping(
        &mut self,
        options: Value,
        fallback: &str,
    ) -> Result<UseGrouping, Thrown> {
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
        self.preflight_native_iteration_work(s.len() as u64)?;
        if s == "true" || s == "false" {
            return Ok(UseGrouping::Str(fallback.to_string()));
        }
        if !["min2", "auto", "always"].contains(&s.as_str()) {
            return Err(Thrown(format!(
                "RangeError: invalid useGrouping value: {s}"
            )));
        }
        Ok(UseGrouping::Str(s))
    }

    /// Intl.NumberFormat.prototype.format(value).
    pub(crate) fn intl_number_format(
        &mut self,
        resolved: u32,
        value: Value,
    ) -> Result<Value, Thrown> {
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
    pub(crate) fn intl_number_format_str(
        &mut self,
        resolved: u32,
        n: f64,
    ) -> Result<String, Thrown> {
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
            let ints: Vec<&String> = parts
                .iter()
                .filter(|(t, _)| t == "integer")
                .map(|(_, v)| v)
                .collect();
            let one =
                ints.len() == 1 && ints[0] == "1" && !parts.iter().any(|(t, _)| t == "fraction");
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
            let seps = numbering_separators(&ns);
            for (ty, v) in parts.iter_mut() {
                match ty.as_str() {
                    "integer" | "fraction" | "exponentInteger" => *v = translate_digits(v, &ns),
                    "decimal" => {
                        if let Some((d, _)) = seps {
                            *v = d.to_string();
                        }
                    }
                    "group" => {
                        if let Some((_, g)) = seps {
                            *v = g.to_string();
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(parts)
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
const HANIDEC_DIGITS: [char; 10] = [
    '\u{3007}', '\u{4e00}', '\u{4e8c}', '\u{4e09}', '\u{56db}', '\u{4e94}', '\u{516d}', '\u{4e03}',
    '\u{516b}', '\u{4e5d}',
];

pub(crate) const AVAILABLE_NUMBERING_SYSTEMS: &[&str] = &[
    "adlm", "ahom", "arab", "arabext", "bali", "beng", "bhks", "brah", "cakm", "cham", "deva",
    "diak", "fullwide", "gara", "gong", "gonm", "gujr", "gukh", "guru", "hanidec", "hmng", "hmnp",
    "java", "kali", "kawi", "khmr", "knda", "krai", "lana", "lanatham", "laoo", "latn", "lepc",
    "limb", "mathbold", "mathdbl", "mathmono", "mathsanb", "mathsans", "mlym", "modi", "mong",
    "mroo", "mtei", "mymr", "mymrepka", "mymrpao", "mymrshan", "mymrtlng", "nagm", "newa", "nkoo",
    "olck", "onao", "orya", "osma", "outlined", "rohg", "saur", "segment", "shrd", "sind", "sinh",
    "sora", "sund", "sunu", "takr", "talu", "tamldec", "telu", "thai", "tibt", "tirh", "tnsa",
    "tols", "vaii", "wara", "wcho",
];

/// The `(decimal, group)` separators a numbering system overrides in the CLDR
/// **root** locale, i.e. independently of which locale is formatting.
///
/// Source: CLDR 47 `common/main/root.xml`, `<numbers><symbols numberSystem=…>`.
/// Of the 75 numbering systems root names, exactly TWO carry their own
/// separators — `arab` and `arabext`, both U+066B ARABIC DECIMAL SEPARATOR and
/// U+066C ARABIC THOUSANDS SEPARATOR; every other system inherits `latn`'s
/// "." / ",". Verified value-by-value against node 24 (ICU 77.1, CLDR 47) over
/// `latn, arab, arabext, deva, thai, hanidec, beng, fullwide` — all eight agree,
/// and only the two Arabic ones differ from ASCII.
///
/// This is NOT locale content: it does not vary with the locale, which is why
/// `en-u-nu-arab` gets `٫` even though CLDR `en` itself only ships `latn`
/// symbols (`DateTimeFormat/prototype/format/numbering-system.js` requires
/// exactly that).
pub(crate) fn numbering_separators(ns: &str) -> Option<(&'static str, &'static str)> {
    match ns {
        "arab" | "arabext" => Some(("\u{66b}", "\u{66c}")),
        _ => None,
    }
}

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
                if c.is_ascii_digit() {
                    HANIDEC_DIGITS[(c as u8 - b'0') as usize]
                } else {
                    c
                }
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
    let neg = crate::vm::cldr_en::PATTERN_ACCOUNTING
        .split(';')
        .nth(1)
        .unwrap_or("");
    let body = |c: char| matches!(c, '¤' | '#' | '0' | ',' | '.');
    let start = neg.find(body).unwrap_or(neg.len());
    let end = neg.rfind(body).map_or(neg.len(), |i| {
        i + neg[i..].chars().next().unwrap().len_utf8()
    });
    (&neg[..start], &neg[end..])
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
        if one {
            row.2.to_string()
        } else {
            row.3.to_string()
        }
    };
    if let Some(r) = d::UNIT_PER_PATTERNS
        .iter()
        .find(|(u, w, ..)| *u == unit && *w == width)
    {
        return Some(pick(r));
    }
    if let Some(r) = d::UNIT_PATTERNS
        .iter()
        .find(|(u, w, ..)| *u == unit && *w == width)
    {
        return Some(pick(r));
    }
    let (num, den) = unit.split_once("-per-")?;
    let num_pat = d::UNIT_PATTERNS
        .iter()
        .find(|(u, w, ..)| *u == num && *w == width)
        .map(pick)?;
    if let Some((.., p)) = d::UNIT_PER_UNIT
        .iter()
        .find(|(u, w, _)| *u == den && *w == width)
    {
        return Some(p.replace("{0}", &num_pat));
    }
    // The denominator contributes its NAME only: its own pattern minus the
    // `{0}` placeholder and the spacing that separated them.
    let den_pat = d::UNIT_PATTERNS
        .iter()
        .find(|(u, w, ..)| *u == den && *w == width)?;
    let den_name = den_pat.2.replace("{0}", "");
    let idx = ["long", "short", "narrow"]
        .iter()
        .position(|w| *w == width)?;
    Some(
        d::UNIT_COMPOUND_PER[idx]
            .replace("{0}", &num_pat)
            .replace("{1}", den_name.trim()),
    )
}
