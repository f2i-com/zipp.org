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
            "year",
            "month",
            "week",
            "day",
            "hour",
            "minute",
            "second",
            "millisecond",
            "microsecond",
            "nanosecond",
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
                    opts.set(
                        "maximumFractionDigits",
                        Value::num(frac.unwrap_or(9) as f64),
                    );
                    opts.set(
                        "minimumFractionDigits",
                        Value::num(frac.unwrap_or(0) as f64),
                    );
                    let tv = self.alloc_str("trunc".to_string());
                    opts.set("roundingMode", tv);
                    done = true;
                }
            }
            // Step 9.f: zero minutes still print inside a numeric run when a
            // seconds field will follow them ("1:00:30", not "1:30").
            let mut display_required = false;
            if i == 5 && need_separator {
                display_required = self.display(self.intl_slot(resolved, "secondsDisplay"))
                    == "always"
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
            let mut run: Vec<(String, String, &'static str)> = self
                .nf_parts(nf_resolved, n)?
                .into_iter()
                .map(|(t, v)| (t, v, nf_unit))
                .collect();
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
        let list_style = if base_style == "digital" {
            "short"
        } else {
            base_style.as_str()
        };
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
}

/// The sanctioned single-unit identifiers of ECMA-402 Table 2. `unit` may name
/// one of these or a `<numerator>-per-<denominator>` pair of them.
pub(crate) const SANCTIONED_UNITS: &[&str] = &[
    "acre",
    "bit",
    "byte",
    "celsius",
    "centimeter",
    "day",
    "degree",
    "fluid-ounce",
    "foot",
    "gallon",
    "gigabit",
    "gigabyte",
    "gram",
    "hectare",
    "hour",
    "inch",
    "kilobit",
    "kilobyte",
    "kilogram",
    "kilometer",
    "liter",
    "megabit",
    "megabyte",
    "meter",
    "microsecond",
    "mile",
    "mile-scandinavian",
    "milliliter",
    "millimeter",
    "millisecond",
    "minute",
    "month",
    "nanosecond",
    "ounce",
    "percent",
    "petabyte",
    "pound",
    "second",
    "stone",
    "terabit",
    "terabyte",
    "week",
    "yard",
    "year",
    "fahrenheit",
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
