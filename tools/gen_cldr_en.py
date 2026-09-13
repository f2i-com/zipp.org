#!/usr/bin/env python3
"""Generate compact locale tables from the upstream CLDR JSON release.

Most Intl services ship en/en-US. DateTimeFormat ships English, German,
Japanese, Chinese and Egyptian Arabic tables from CLDR 48. Use
`--locale de --date-time-only` for that service's German tables. Every string
below is copied verbatim out of the CLDR release
named by `--version`; nothing here is hand-written, translated or guessed.

Inputs (download side by side into one directory, or pass a cldr-json checkout):

    cldr-core/supplemental/dayPeriods.json          flexible day-period rules
    cldr-dates-full/main/en/ca-gregorian.json       names + date/time patterns
    cldr-dates-full/main/en/dateFields.json         RelativeTimeFormat patterns
    cldr-numbers-full/main/en/numbers.json          symbols + number patterns
    cldr-units-full/main/en/units.json              NumberFormat unit names
    cldr-misc-full/main/en/listPatterns.json        ListFormat patterns

    curl -O https://raw.githubusercontent.com/unicode-org/cldr-json/47.0.0/cldr-json/cldr-core/supplemental/dayPeriods.json
    ... etc ...

Usage:

    python tools/gen_cldr_en.py <dir-with-the-json> --version 47.0.0 \
        -o crates/zipp-vm/src/vm/cldr_en.rs

    python tools/gen_cldr_en.py <dir-with-German-json> --version 48.0.0 \
        --locale de --date-time-only -o crates/zipp-vm/src/vm/cldr_de.rs

The legacy full English table remains on CLDR 47. DateTimeFormat additionally
reads supplemental timeData/likelySubtags, timeZoneNames and calendar JSONs;
Japanese and Chinese also need cldr-rbnf's locale rbnf.json for date-style
number annotations. Generated headers list every input and its source hash.
Node comparisons must account for the ICU/CLDR version linked by that build.
"""

import argparse
import re
import hashlib
import json
import os
import sys

# ── ECMA-402 calendar id -> the CLDR package that carries its names ─────────
# The ids are ECMA-402's; several share one CLDR file (see the note in main()).
CAL_ID_SOURCE = [
    ("buddhist", "buddhist"),
    ("chinese", "chinese"),
    ("coptic", "coptic"),
    ("dangi", "dangi"),
    ("ethioaa", "ethiopic"),
    ("ethiopic", "ethiopic"),
    ("hebrew", "hebrew"),
    ("indian", "indian"),
    ("islamic-civil", "islamic"),
    ("islamic-tbla", "islamic"),
    ("islamic-umalqura", "islamic"),
    ("japanese", "japanese"),
    ("persian", "persian"),
    ("roc", "roc"),
]

# ── ECMA-402 Table 2, the sanctioned single units ───────────────────────────
# `style:"unit"` accepts only these and `<a>-per-<b>` pairs of them, so the unit
# names below are a closed 45-row subset of CLDR's ~225 — the rest of
# `units.json` is unreachable from JavaScript and is not emitted.
SANCTIONED = [
    "acre", "bit", "byte", "celsius", "centimeter", "day", "degree", "fahrenheit",
    "fluid-ounce", "foot", "gallon", "gigabit", "gigabyte", "gram", "hectare", "hour",
    "inch", "kilobit", "kilobyte", "kilogram", "kilometer", "liter", "megabit",
    "megabyte", "meter", "microsecond", "mile", "mile-scandinavian", "milliliter",
    "millimeter", "millisecond", "minute", "month", "nanosecond", "ounce", "percent",
    "petabyte", "pound", "second", "stone", "terabit", "terabyte", "week", "yard",
    "year",
]

WIDTHS = ["long", "short", "narrow"]
STYLES4 = ["full", "long", "medium", "short"]


def rs(s):
    """A Rust string literal for an arbitrary CLDR string.

    CLDR patterns contain ` `/` ` (narrow/thin spaces) and `'`-quoted
    literal runs; both must survive byte-exact, so escape rather than prettify.
    """
    out = ['"']
    for ch in s:
        if ch == "\\":
            out.append("\\\\")
        elif ch == '"':
            out.append('\\"')
        elif ch == "\n":
            out.append("\\n")
        elif ord(ch) < 0x20 or ord(ch) == 0x7F:
            out.append("\\u{%x}" % ord(ch))
        else:
            out.append(ch)
    out.append('"')
    return "".join(out)


def load(root, *rel):
    for r in rel:
        p = os.path.join(root, r)
        if os.path.exists(p):
            return json.load(open(p, encoding="utf-8")), p
    sys.exit("missing input: none of %s under %s" % (", ".join(rel), root))


def lunar_day_from_rbnf(rules, name, value, depth=0):
    """Expand the pinned CLDR small-integer rules into a finite day-name table."""
    if not 0 <= value <= 31 or depth > 12:
        raise ValueError("unsupported lunar-day RBNF input")
    base, pattern = max((int(k), p) for k, p in rules[name] if k.isdigit() and int(k) <= value)
    divisor = 10 ** (len(str(base)) - 1) if base else 1
    remainder = value % divisor
    pattern = pattern.removesuffix(';')
    pattern = re.sub(r'\[([^\[\]]*)\]', lambda m: m[1] if remainder else '', pattern)
    pattern = re.sub(r'=([^=]+)=', lambda m: lunar_day_from_rbnf(rules, m[1], value, depth + 1), pattern)
    if '←←' in pattern:
        pattern = pattern.replace('←←', lunar_day_from_rbnf(rules, name, value // divisor, depth + 1))
    if '→→' in pattern:
        pattern = pattern.replace('→→', lunar_day_from_rbnf(rules, name, remainder, depth + 1))
    if any(c in pattern for c in '←→=[];'):
        raise ValueError("unsupported lunar-day RBNF syntax")
    return pattern


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("root", help="directory holding the CLDR JSON files")
    ap.add_argument("--date-time-only", action="store_true", help="emit only DateTimeFormat data")
    ap.add_argument("--locale", default="en", choices=["en", "de", "ja", "zh", "ar-EG"], help="CLDR locale to emit")
    ap.add_argument("--version", required=True, help="CLDR release, e.g. 47.0.0")
    ap.add_argument("-o", "--out", required=True)
    a = ap.parse_args()

    locale = a.locale
    src_digests = []

    def get(*rel):
        obj, p = load(a.root, *rel)
        h = hashlib.sha256(open(p, "rb").read()).hexdigest()[:16]
        src_digests.append((os.path.basename(p), h))
        return obj

    gregorian = get("ca-gregorian.json", f"cldr-dates-full/main/{locale}/ca-gregorian.json")
    gregorian = gregorian["main"][locale]["dates"]["calendars"]["gregorian"]
    dayperiods = get("dayPeriods.json", "cldr-core/supplemental/dayPeriods.json")
    dp_rules = dayperiods["supplemental"]["dayPeriodRuleSet"][locale.split('-')[0]]
    fields = get("dateFields.json", f"cldr-dates-full/main/{locale}/dateFields.json")
    fields = fields["main"][locale]["dates"]["fields"]
    numbers = get("numbers.json", f"cldr-numbers-full/main/{locale}/numbers.json")
    numbers = numbers["main"][locale]["numbers"]
    if not a.date_time_only:
        units = get("units.json", f"cldr-units-full/main/{locale}/units.json")
        units = units["main"][locale]["units"]
        lists = get("listPatterns.json", f"cldr-misc-full/main/{locale}/listPatterns.json")
        lists = lists["main"][locale]["listPatterns"]
        ldn = get("localeDisplayNames.json", f"cldr-localenames-full/main/{locale}/localeDisplayNames.json")
        ldn = ldn["main"][locale]["localeDisplayNames"]
    # ── the non-gregorian calendars Intl.DateTimeFormat can resolve ──────────
    # One CLDR package per calendar (`cldr-cal-<x>-full/main/en/ca-<x>.json`).
    # `islamic` covers the three variants ECMA-402 lists separately
    # (islamic-civil / islamic-tbla / islamic-umalqura) — they differ only in
    # their day arithmetic, which lives in vm/temporal, never in their names.
    # `ethioaa` (ethiopic-amete-alem) shares ethiopic's month names and differs
    # only in era, so it reads the same file.
    cal_files = ["buddhist", "chinese", "coptic", "dangi", "ethiopic", "hebrew",
                 "indian", "islamic", "japanese", "persian", "roc"]
    cal_data = {}
    for c in cal_files:
        obj = get("ca-%s.json" % c, "cldr-cal-%s-full/main/%s/ca-%s.json" % (c, locale, c))
        cal_data[c] = obj["main"][locale]["dates"]["calendars"][c]

    if a.date_time_only:
        zones = get("timeZoneNames.json", f"cldr-dates-full/main/{locale}/timeZoneNames.json")["main"][locale]["dates"]["timeZoneNames"]
        time_data = get("timeData.json", "cldr-core/supplemental/timeData.json")["supplemental"]["timeData"]
        likely = get("likelySubtags.json", "cldr-core/supplemental/likelySubtags.json")["supplemental"]["likelySubtags"]
        territory = next((s for s in locale.split('-')[1:] if len(s) == 2 and s.isupper()), None)
        if territory is None:
            territory = likely[locale.split('-')[0]].replace('_', '-').split('-')[-1]
        clock = time_data[territory]
        cycles = {'h': 'h12', 'H': 'h23', 'K': 'h11', 'k': 'h24'}
        hour_cycle = cycles[clock['_preferred']]
        hour_cycle12 = next(cycles[s[0]] for s in clock['_allowed'].split() if s[0] in 'hK')
        first_year = ''
        lunar_days = []
        if locale in ['ja', 'zh']:
            rules = get('rbnf.json', f'cldr-rbnf/rbnf/{locale}.json')['rbnf']['rbnf']['SpelloutRules']
            if locale == 'ja':
                first_year = dict(rules['%spellout-numbering-year-latn'])['1'].removesuffix(';')
            else:
                lunar_days = [lunar_day_from_rbnf(rules, '%spellout-numbering-days', day) for day in range(1, 31)]

    def pattern_value(mapping, key):
        # CLDR explicitly supplies ASCII alternatives for spacing around AM/PM.
        # Use that supported presentation consistently in format and parts.
        value = mapping.get(key + '-alt-ascii', mapping[key]) if a.date_time_only else mapping[key]
        return value['_value'] if isinstance(value, dict) else value

    L = []
    w = L.append

    w(f"//! CLDR `{locale}` locale content — GENERATED by `tools/gen_cldr_en.py`, do not edit.")
    w("//!")
    w("//! Source: CLDR release %s (https://github.com/unicode-org/cldr-json, tag %s),"
      % (a.version, a.version))
    w("//! files (sha256, first 16 hex):")
    for name, h in src_digests:
        w("//!   %-24s %s" % (name, h))
    w("//!")
    w("//! Locale availability is service-specific; these are upstream data, not")
    w("//! a promise that every Intl service supports this locale. See intl/shared.rs.")
    w("#![allow(dead_code)]")
    w("")
    w("/// The CLDR release these tables were cut from.")
    w("pub const CLDR_VERSION: &str = %s;" % rs(a.version))
    w("")

    # ── gregorian names ────────────────────────────────────────────────────
    def names(node, kind, width, n, base):
        d = node[kind][width]
        return [d[str(i)] for i in range(base, base + n)]

    w("// ── gregorian calendar names (ca-gregorian.json → dates.calendars.gregorian)")
    for form, tag in (("format", ""), ("stand-alone", "_SA")):
        for width, suffix in (("wide", "WIDE"), ("abbreviated", "ABBR"), ("narrow", "NARROW")):
            v = names(gregorian["months"], form, width, 12, 1)
            w("pub const MONTHS%s_%s: [&str; 12] = [%s];" % (tag, suffix, ", ".join(rs(x) for x in v)))
    DAYKEYS = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"]
    for form, tag in (("format", ""), ("stand-alone", "_SA")):
        for width, suffix in (("wide", "WIDE"), ("abbreviated", "ABBR"),
                              ("short", "SHORT"), ("narrow", "NARROW")):
            d = gregorian["days"][form][width]
            v = [d[k] for k in DAYKEYS]
            w("pub const DAYS%s_%s: [&str; 7] = [%s];" % (tag, suffix, ", ".join(rs(x) for x in v)))
    for form, tag in (("format", ""), ("stand-alone", "_SA")):
        for width, suffix in (("wide", "WIDE"), ("abbreviated", "ABBR"), ("narrow", "NARROW")):
            v = names(gregorian["quarters"], form, width, 4, 1)
            w("pub const QUARTERS%s_%s: [&str; 4] = [%s];" % (tag, suffix, ", ".join(rs(x) for x in v)))
    w("")
    w("/// Eras indexed by CLDR era number: 0 = BC, 1 = AD.")
    for key, suffix in (("eraNames", "WIDE"), ("eraAbbr", "ABBR"), ("eraNarrow", "NARROW")):
        d = gregorian["eras"][key]
        w("pub const ERAS_%s: [&str; 2] = [%s, %s];" % (suffix, rs(d["0"]), rs(d["1"])))
    w("")

    # ── day periods ────────────────────────────────────────────────────────
    dp_keys = ["midnight", "am", "noon", "pm", "morning1", "morning2",
               "afternoon1", "afternoon2", "evening1", "evening2", "night1", "night2"]
    present = [k for k in dp_keys if k in gregorian["dayPeriods"]["format"]["wide"]]
    w("/// Day-period names, `(key, wide, abbreviated, narrow)`. `am`/`pm` are the")
    w("/// fixed pair a `h`/`K` pattern's `a` field prints; the rest are the FLEXIBLE")
    w("/// periods `B` prints, selected by `DAY_PERIOD_RULES`.")
    # ── non-gregorian calendar names ────────────────────────────────────────
    # Emitted as one flat table keyed by the ECMA-402 calendar id, so the
    # formatter looks up (calendar, width) and gets a slice. Month lists are
    # 1-based and dense: a calendar with 13 months emits 13 entries. Hebrew's
    # "7-yeartype-leap" (Adar II) is emitted as a SEPARATE 14th entry because in
    # a leap year month 7 renames and months 8..13 shift — vm/temporal already
    # models that shift, so the formatter only needs the extra name.
    def cal_months(node, width):
        d = node["months"]["format"][width]
        keys = [k for k in d if k.isdigit()]
        out = [d[str(i)] for i in range(1, len(keys) + 1)]
        if "7-yeartype-leap" in d:
            out.append(d["7-yeartype-leap"])
        return out

    def cal_eras(node, kind):
        d = node.get("eras", {}).get(kind, {})
        keys = sorted((int(k) for k in d if k.isdigit()))
        return [d[str(k)] for k in keys]

    w("/// Month names per non-gregorian calendar, 1-based and dense.")
    w("/// Hebrew carries a 14th entry: Adar II, used only in a leap year.")
    w("/// `(calendar, wide, abbreviated, narrow)`.")
    w("pub const CAL_MONTHS: &[(&str, &[&str], &[&str], &[&str])] = &[")
    for cid, src in CAL_ID_SOURCE:
        node = cal_data[src]
        w("    (%s, &[%s], &[%s], &[%s])," % (
            rs(cid),
            ", ".join(rs(x) for x in cal_months(node, "wide")),
            ", ".join(rs(x) for x in cal_months(node, "abbreviated")),
            ", ".join(rs(x) for x in cal_months(node, "narrow")),
        ))
    w("];")
    w("")
    w("/// Era names per non-gregorian calendar, indexed by the era ORDINAL that")
    w("/// `vm::temporal::calendar::cal_era` returns. `(calendar, wide, abbr, narrow)`.")
    w("pub const CAL_ERAS: &[(&str, &[&str], &[&str], &[&str])] = &[")
    for cid, src in CAL_ID_SOURCE:
        node = cal_data[src]
        w("    (%s, &[%s], &[%s], &[%s])," % (
            rs(cid),
            ", ".join(rs(x) for x in cal_eras(node, "eraNames")),
            ", ".join(rs(x) for x in cal_eras(node, "eraAbbr")),
            ", ".join(rs(x) for x in cal_eras(node, "eraNarrow")),
        ))
    w("];")
    w("")
    w("/// The four `dateStyle` patterns per non-gregorian calendar, in")
    w("/// full/long/medium/short order. Most calendars differ from gregorian only")
    w("/// by wanting an era; hebrew is day-first; chinese and dangi use `r(U)`")
    w("/// (related ISO year + cyclic year name).")
    w("pub const CAL_DATE_FORMATS: &[(&str, [&str; 4])] = &[")
    for cid, src in CAL_ID_SOURCE:
        df = cal_data[src]["dateFormats"]
        w("    (%s, [%s, %s, %s, %s])," % (
            rs(cid), *(rs(pattern_value(df, style)) for style in STYLES4)))
    w("];")
    w("")
    w("pub const DAY_PERIODS: &[(&str, &str, &str, &str)] = &[")
    for k in present:
        row = [gregorian["dayPeriods"]["format"][x][k] for x in ("wide", "abbreviated", "narrow")]
        w("    (%s, %s, %s, %s)," % (rs(k), rs(row[0]), rs(row[1]), rs(row[2])))
    w("];")
    w("")

    def hm(t):
        h, m = t.split(":")
        return int(h) * 60 + int(m)

    w(f"/// UTS #35 §4.7 flexible day-period rules for `{locale}` (supplemental/dayPeriods.json).")
    w("/// `(key, at, from, before)` in minutes past local midnight; `at` is -1 for a")
    w("/// range rule, and `from`/`before` are -1 for an instant rule. `at` rules win.")
    w("pub const DAY_PERIOD_RULES: &[(&str, i32, i32, i32)] = &[")
    for k, r in sorted(dp_rules.items()):
        at = hm(r["_at"]) if "_at" in r else -1
        fr = hm(r["_from"]) if "_from" in r else -1
        be = hm(r["_before"]) if "_before" in r else -1
        w("    (%s, %d, %d, %d)," % (rs(k), at, fr, be))
    w("];")
    w("")

    # ── date/time patterns ─────────────────────────────────────────────────
    w("// ── date/time patterns, indexed [full, long, medium, short]")
    w("pub const DATE_FORMATS: [&str; 4] = [%s];"
      % ", ".join(rs(pattern_value(gregorian["dateFormats"], s)) for s in STYLES4))
    w("pub const TIME_FORMATS: [&str; 4] = [%s];"
      % ", ".join(rs(pattern_value(gregorian["timeFormats"], s)) for s in STYLES4))
    w("/// The `{1} … {0}` glue joining a date pattern to a time pattern.")
    w("pub const DATETIME_GLUE: [&str; 4] = [%s];"
      % ", ".join(rs(gregorian["dateTimeFormats"][s]) for s in STYLES4))
    at = gregorian.get("dateTimeFormats-atTime", {}).get("standard")
    if at:
        w("/// The `atTime` glue CLDR 42+ uses when a dateStyle/timeStyle pair is joined")
        w("/// (`\"{1} 'at' {0}\"` for full/long in `en`); ICU picks this over the plain")
        w("/// glue for dateStyle+timeStyle, which is why `timedatestyle-en.js` accepts")
        w("/// both spellings.")
        w("pub const DATETIME_GLUE_AT: [&str; 4] = [%s];"
          % ", ".join(rs(at[s]) for s in STYLES4))
    w("")
    w("/// `availableFormats`: skeleton → pattern. The skeleton is the canonical field")
    w("/// set a request is matched against (UTS #35 §4.5 \"Matching Skeletons\").")
    w("pub const AVAILABLE_FORMATS: &[(&str, &str)] = &[")
    for k, v in sorted(gregorian["dateTimeFormats"]["availableFormats"].items()):
        if "-alt-" in k:
            continue  # `-alt-ascii` duplicates the default with ASCII spacing
        w("    (%s, %s)," % (rs(k), rs(pattern_value(gregorian["dateTimeFormats"]["availableFormats"], k))))
    w("];")
    w("")
    w("/// `appendItems`: how a requested field that the matched pattern does not")
    w("/// carry is attached to it — `(field letter, glue, field display name)`.")
    w("/// `{0}` is the pattern so far, `{1}` the new field, `{2}` its name, so")
    w("/// `{year, day}` in `en` prints \"1886 (day: 1)\".")
    APPEND = [("G", "Era", "era"), ("y", "Year", "year"), ("Q", "Quarter", "quarter"),
              ("M", "Month", "month"), ("w", "Week", "week"), ("d", "Day", "day"),
              ("E", "Day-Of-Week", "weekday"), ("h", "Hour", "hour"),
              ("m", "Minute", "minute"), ("s", "Second", "second"),
              ("z", "Timezone", "zone")]
    w("pub const APPEND_ITEMS: &[(&str, &str, &str)] = &[")
    ai = gregorian["dateTimeFormats"]["appendItems"]
    for letter, key, field in APPEND:
        w("    (%s, %s, %s)," % (rs(letter), rs(ai[key]), rs(fields[field]["displayName"])))
    w("];")
    w("")
    w("/// `intervalFormats`: (skeleton, greatest-difference field, pattern). The")
    w("/// pattern names each field twice, once per endpoint.")
    w("pub const INTERVAL_FORMATS: &[(&str, &str, &str)] = &[")
    ivf = gregorian["dateTimeFormats"]["intervalFormats"]
    for sk in sorted(k for k in ivf if k != "intervalFormatFallback"):
        if "-alt-" in sk:
            continue
        for gd in sorted(ivf[sk]):
            w("    (%s, %s, %s)," % (rs(sk), rs(gd), rs(ivf[sk][gd])))
    w("];")
    w("/// Used when no interval pattern matches: format both endpoints and join.")
    w("pub const INTERVAL_FALLBACK: &str = %s;" % rs(ivf["intervalFormatFallback"]))
    w("")

    if a.date_time_only:
        w("pub const JAPANESE_FIRST_YEAR: &str = %s;" % rs(first_year))
        w("pub const LUNAR_DAY_NAMES: &[&str] = &[%s];" % ', '.join(rs(day) for day in lunar_days))
        w("pub const CAL_DATE_NUMBER_SYSTEMS: &[(&str, [&str; 4])] = &[")
        for cid, src in CAL_ID_SOURCE:
            df = cal_data[src]['dateFormats']
            overrides = [df[s].get('_numbers', '') if isinstance(df[s], dict) else '' for s in STYLES4]
            if any(overrides):
                w("    (%s, [%s])," % (rs(cid), ', '.join(rs(s) for s in overrides)))
        w("];")
        w("/// Calendar-specific skeletons and intervals; gregorian is not a universal template.")
        w("pub const CAL_PATTERNS: &[super::dtf_locale::CalendarPatterns] = &[")
        for cid, src in CAL_ID_SOURCE:
            node = cal_data[src]
            formats = node['dateTimeFormats']
            w("    super::dtf_locale::CalendarPatterns {")
            w("        id: %s," % rs(cid))
            w("        available_formats: &[")
            for key in sorted(formats['availableFormats']):
                if '-alt-' not in key:
                    w("            (%s, %s)," % (rs(key), rs(pattern_value(formats['availableFormats'], key))))
            w("        ],")
            w("        interval_formats: &[")
            intervals = formats['intervalFormats']
            for key in sorted(intervals):
                if key != 'intervalFormatFallback' and '-alt-' not in key:
                    for greatest in sorted(intervals[key]):
                        w("            (%s, %s, %s)," % (rs(key), rs(greatest), rs(intervals[key][greatest])))
            w("        ],")
            w("        interval_fallback: %s," % rs(intervals['intervalFormatFallback']))
            glue = node.get('dateTimeFormats-atTime', {}).get('standard', formats)
            w("        datetime_glue_at: [%s]," % ', '.join(rs(glue[s]) for s in STYLES4))
            w("    },")
        w("];")
        w("pub const CYCLIC_YEARS: &[(&str, &[&str])] = &[")
        for cid, src in CAL_ID_SOURCE:
            cyclic = cal_data[src].get('cyclicNameSets', {}).get('years', {}).get('format', {}).get('abbreviated')
            if cyclic:
                w("    (%s, &[%s])," % (rs(cid), ', '.join(rs(cyclic[str(i)]) for i in range(1, 61))))
        w("];")
        w("/// Leap-month templates in wide/abbreviated/narrow/numeric order, for both contexts.")
        w("pub const LEAP_MONTH_PATTERNS: &[(&str, [&str; 4], [&str; 4])] = &[")
        for cid, src in CAL_ID_SOURCE:
            leap = cal_data[src].get('monthPatterns')
            if leap:
                forms = []
                for form in ['format', 'stand-alone']:
                    forms.append('[' + ', '.join(rs(leap[form][width]['leap']) for width in ['wide','abbreviated','narrow']) + ', ' + rs(leap['numeric']['all']['leap']) + ']')
                w("    (%s, %s, %s)," % (rs(cid), *forms))
        w("];")
        w("pub const DEFAULT_HOUR_CYCLE: &str = %s;" % rs(hour_cycle))
        w("pub const DEFAULT_HOUR_CYCLE12: &str = %s;" % rs(hour_cycle12))
        w("pub const DEFAULT_NUMBERING_SYSTEM: &str = %s;" % rs(numbers['defaultNumberingSystem']))
        w("pub const SYM_DECIMAL: &str = %s;" % rs(numbers["symbols-numberSystem-latn"]["decimal"]))
        w("pub const UTC_LONG: &str = %s;" % rs(zones["zone"]["Etc"]["UTC"]["long"]["standard"]))
        open(a.out, "w", encoding="utf-8", newline="\n").write("\n".join(L) + "\n")
        print("wrote %s (%d lines)" % (a.out, len(L)))
        return

    # ── list patterns ──────────────────────────────────────────────────────
    w("/// ListFormat patterns: `(type, style, two, start, middle, end)`.")
    w("pub const LIST_PATTERNS: &[(&str, &str, &str, &str, &str, &str)] = &[")
    for ty, cldr_ty in (("conjunction", "standard"), ("disjunction", "or"), ("unit", "unit")):
        for style in ("long", "short", "narrow"):
            key = "listPattern-type-" + cldr_ty + ("" if style == "long" else "-" + style)
            p = lists[key]
            w("    (%s, %s, %s, %s, %s, %s)," % (rs(ty), rs(style), rs(p["2"]),
                                                 rs(p["start"]), rs(p["middle"]), rs(p["end"])))
    w("];")
    w("")

    # ── display names ──────────────────────────────────────────────────────
    # `Intl.DisplayNames(… {type: "calendar"})`. The BCP-47 key is `ca` and its
    # `gregory` type is spelled `gregorian` in the display-name table, so the
    # row is emitted under the identifier JavaScript passes.
    w("/// `Intl.DisplayNames` calendar names, keyed by the BCP-47 calendar id.")
    w("pub const CALENDAR_NAMES: &[(&str, &str)] = &[")
    cal_alias = {"gregorian": "gregory"}
    for k, v in sorted(ldn["types"]["calendar"].items()):
        w("    (%s, %s)," % (rs(cal_alias.get(k, k)), rs(v)))
    w("];")
    w("")

    # ── relative time ──────────────────────────────────────────────────────
    w("/// RelativeTimeFormat patterns (dateFields.json), one row per unit × style:")
    w("/// `(unit, style, future_one, future_other, past_one, past_other)`.")
    w("pub const RELATIVE_PATTERNS: &[(&str, &str, &str, &str, &str, &str)] = &[")
    RT_UNITS = ["second", "minute", "hour", "day", "week", "month", "quarter", "year"]
    for u in RT_UNITS:
        for style in ("long", "short", "narrow"):
            key = u + ("" if style == "long" else "-" + style)
            f = fields[key]["relativeTime-type-future"]
            p = fields[key]["relativeTime-type-past"]
            w("    (%s, %s, %s, %s, %s, %s)," % (
                rs(u), rs(style),
                rs(f["relativeTimePattern-count-one"]), rs(f["relativeTimePattern-count-other"]),
                rs(p["relativeTimePattern-count-one"]), rs(p["relativeTimePattern-count-other"])))
    w("];")
    w("")
    w("/// The `numeric: \"auto\"` literals — CLDR `relative-type-<n>`, e.g. \"yesterday\",")
    w("/// \"now\", \"next quarter\". `(unit, style, offset, text)`; a unit/offset absent")
    w("/// here has no idiomatic form and falls back to the numeric pattern.")
    w("pub const RELATIVE_LITERALS: &[(&str, &str, i32, &str)] = &[")
    for u in RT_UNITS:
        for style in ("long", "short", "narrow"):
            key = u + ("" if style == "long" else "-" + style)
            for off in (-2, -1, 0, 1, 2):
                t = fields[key].get("relative-type-%d" % off)
                if t is not None:
                    w("    (%s, %s, %d, %s)," % (rs(u), rs(style), off, rs(t)))
    w("];")
    w("")

    # ── number symbols & patterns ──────────────────────────────────────────
    sym = numbers["symbols-numberSystem-latn"]
    w("// ── number symbols and patterns (numbers.json, numberSystem=latn)")
    for js, cldr in (("DECIMAL", "decimal"), ("GROUP", "group"), ("PERCENT_SIGN", "percentSign"),
                     ("PLUS_SIGN", "plusSign"), ("MINUS_SIGN", "minusSign"),
                     ("APPROX_SIGN", "approximatelySign"), ("EXPONENTIAL", "exponential"),
                     ("NAN", "nan"), ("INFINITY", "infinity")):
        w("pub const SYM_%s: &str = %s;" % (js, rs(sym[cldr])))
    w("pub const PATTERN_DECIMAL: &str = %s;" % rs(numbers["decimalFormats-numberSystem-latn"]["standard"]))
    w("pub const PATTERN_PERCENT: &str = %s;" % rs(numbers["percentFormats-numberSystem-latn"]["standard"]))
    cur = numbers["currencyFormats-numberSystem-latn"]
    w("pub const PATTERN_CURRENCY: &str = %s;" % rs(cur["standard"]))
    w("/// `signDisplay` and the currencySign:\"accounting\" pattern share this row: its")
    w("/// negative subpattern is what wraps a negative amount in parentheses.")
    w("pub const PATTERN_ACCOUNTING: &str = %s;" % rs(cur["accounting"]))
    misc = numbers.get("miscPatterns-numberSystem-latn", {})
    if "range" in misc:
        w("/// `formatRange`'s joiner and `approximatelySign`'s wrapper.")
        w("pub const PATTERN_RANGE: &str = %s;" % rs(misc["range"]))
    if "approximately" in misc:
        w("pub const PATTERN_APPROX: &str = %s;" % rs(misc["approximately"]))
    w("")
    w("/// Compact decimal patterns: `(power of ten, plural count, pattern)`. The count")
    w("/// of `0`s in the pattern is the number of integer digits it keeps.")
    for style, name in (("short", "SHORT"), ("long", "LONG")):
        d = numbers["decimalFormats-numberSystem-latn"][style]["decimalFormat"]
        w("pub const COMPACT_DECIMAL_%s: &[(u32, &str, &str)] = &[" % name)
        rows = []
        for k, v in d.items():
            mag, _, cnt = k.partition("-count-")
            rows.append((len(mag) - 1, cnt, v))
        for p, c, v in sorted(rows):
            w("    (%d, %s, %s)," % (p, rs(c), rs(v)))
        w("];")
    w("")

    # ── units ──────────────────────────────────────────────────────────────
    # Map an ECMA-402 unit id onto its CLDR key, which carries a category
    # prefix (`length-kilometer`, `concentr-percent`, …). The prefix is opaque
    # to ECMA-402, so it is discovered rather than hard-coded.
    def cldr_key(unit_id, width):
        d = units[width]
        exact = [k for k in d if k.split("-", 1)[-1] == unit_id]
        if len(exact) == 1:
            return exact[0]
        if len(exact) > 1:
            sys.exit("ambiguous CLDR key for %s: %s" % (unit_id, exact))
        return None

    w("/// Unit display patterns for the 45 sanctioned units of ECMA-402 Table 2,")
    w("/// `(unit, width, one, other)`. `{0}` is the formatted number.")
    w("pub const UNIT_PATTERNS: &[(&str, &str, &str, &str)] = &[")
    missing = []
    for u in SANCTIONED:
        for width in WIDTHS:
            k = cldr_key(u, width)
            if k is None:
                missing.append((u, width))
                continue
            d = units[width][k]
            w("    (%s, %s, %s, %s)," % (rs(u), rs(width),
                                         rs(d["unitPattern-count-one"]),
                                         rs(d["unitPattern-count-other"])))
    w("];")
    if missing:
        sys.exit("no CLDR unit row for %s" % missing)
    w("")
    w("/// CLDR carries idiomatic names for many `x-per-y` pairs (`km/h`, not `km/hour`);")
    w("/// where it does, ECMA-402's compound construction must not be used instead.")
    w("/// Only pairs of sanctioned units are reachable, so only those are emitted.")
    w("pub const UNIT_PER_PATTERNS: &[(&str, &str, &str, &str)] = &[")
    for n in SANCTIONED:
        for d_ in SANCTIONED:
            uid = "%s-per-%s" % (n, d_)
            rows = []
            for width in WIDTHS:
                k = cldr_key(uid, width)
                if k is not None:
                    rows.append((width, units[width][k]))
            if len(rows) == len(WIDTHS):
                for width, dd in rows:
                    w("    (%s, %s, %s, %s)," % (rs(uid), rs(width),
                                                 rs(dd["unitPattern-count-one"]),
                                                 rs(dd["unitPattern-count-other"])))
    w("];")
    w("")
    w("/// A DENOMINATOR's `perUnitPattern` — UTS #35 \"Compound Units\" step 2, used")
    w("/// when no idiomatic `x-per-y` row exists: `{0}` takes the whole formatted")
    w("/// numerator, so `byte-per-year` short is \"2 byte\" in \"{0}/y\" = \"2 byte/y\".")
    w("pub const UNIT_PER_UNIT: &[(&str, &str, &str)] = &[")
    for u in SANCTIONED:
        for width in WIDTHS:
            k = cldr_key(u, width)
            p = units[width][k].get("perUnitPattern")
            if p is not None:
                w("    (%s, %s, %s)," % (rs(u), rs(width), rs(p)))
    w("];")
    w("")
    w("/// `compoundUnitPattern`: the last-resort numerator/denominator join when the")
    w("/// denominator has no `perUnitPattern` either. Indexed long/short/narrow.")
    w("pub const UNIT_COMPOUND_PER: [&str; 3] = [%s];"
      % ", ".join(rs(units[w_]["per"]["compoundUnitPattern"]) for w_ in WIDTHS))
    w("")

    open(a.out, "w", encoding="utf-8", newline="\n").write("\n".join(L) + "\n")
    print("wrote %s (%d lines)" % (a.out, len(L)))


if __name__ == "__main__":
    main()
