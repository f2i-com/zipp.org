//! `class D extends <builtin>`: `super()` builds the builtin instance with the
//! real constructor and copies its representation into the subclass's slot.
//!
//! The copy names the representations those constructors can return. One it
//! does not name is an abort, so every guarded family is constructed here, in
//! whichever profile this suite is built for: the typed arrays and DataView,
//! the wrappers, Date and RegExp, the four function constructors, Array, and
//! where the build has them the eight Temporal types and the ten Intl
//! services.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    state.run_init().expect("source runs");
    state.take_output()
}

const CORE: &str = r#"
var out = [];
function t(label, f) { try { out.push(label + "=" + f()); } catch (e) { out.push(label + "!" + e); } }

for (const TA of [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array,
                  Int32Array, Uint32Array, Float32Array, Float64Array, BigInt64Array, BigUint64Array]) {
  t(TA.name, () => {
    class D extends TA { twice() { return this.length * 2; } }
    const d = new D(3);
    d[1] = TA.name.startsWith("Big") ? 7n : 7;
    return [d instanceof D, d instanceof TA, d.length, String(d[1]), d.twice(), d.byteLength > 0].join(",");
  });
}
t("DataView", () => {
  class D extends DataView { first() { return this.getUint8(0); } }
  const d = new D(new Uint8Array([9, 8, 7]).buffer, 1);
  return [d instanceof D, d instanceof DataView, d.byteLength, d.byteOffset, d.first()].join(",");
});
t("Number", () => { class D extends Number { inc() { return this + 1; } } const d = new D(41); return [d instanceof D, d.inc(), d.toFixed(1), typeof d].join(","); });
t("String", () => { class D extends String { shout() { return this.toUpperCase(); } } const d = new D("ab"); return [d instanceof D, d.shout(), d.length, d[1]].join(","); });
t("Boolean", () => { class D extends Boolean {} const d = new D(true); return [d instanceof D, d.valueOf(), typeof d].join(","); });
t("Date", () => { class D extends Date { y() { return this.getUTCFullYear(); } } const d = new D(Date.UTC(2026, 0, 2)); return [d instanceof D, d.y(), d.toISOString()].join(","); });
t("RegExp", () => {
  class D extends RegExp { n(s) { return (s.match(this) || []).length; } }
  const d = new D("a.", "g");
  const first = d.exec("abacad");
  return [d instanceof D, d.source, d.flags, first[0], d.lastIndex, d.n("abacad")].join(",");
});
t("Function", () => { class D extends Function {} const d = new D("a", "b", "return a * b"); return [d instanceof D, d(6, 7), d.length, typeof d].join(","); });
t("GeneratorFunction", () => {
  const G = Object.getPrototypeOf(function* () {}).constructor;
  class D extends G {}
  const d = new D("n", "yield n; yield n + 1");
  return [d instanceof D, [...d(4)].join("+")].join(",");
});
t("AsyncFunction", () => {
  const A = Object.getPrototypeOf(async function () {}).constructor;
  class D extends A {}
  const d = new D("return 5");
  return [d instanceof D, d() instanceof Promise].join(",");
});
t("AsyncGeneratorFunction", () => {
  const A = Object.getPrototypeOf(async function* () {}).constructor;
  class D extends A {}
  const d = new D("yield 1");
  return [d instanceof D, typeof d().next].join(",");
});
t("Array", () => {
  class D extends Array { sum() { return this.reduce((a, b) => a + b, 0); } }
  const d = new D(1, 2, 3);
  d.push(4);
  const e = new D(5);
  return [d instanceof D, Array.isArray(d), d.length, d.sum(), e.length, d.map(x => x * 2) instanceof D].join(",");
});
"#;

const CORE_EXPECTED: &[&str] = &[
    "Int8Array=true,true,3,7,6,true",
    "Uint8Array=true,true,3,7,6,true",
    "Uint8ClampedArray=true,true,3,7,6,true",
    "Int16Array=true,true,3,7,6,true",
    "Uint16Array=true,true,3,7,6,true",
    "Int32Array=true,true,3,7,6,true",
    "Uint32Array=true,true,3,7,6,true",
    "Float32Array=true,true,3,7,6,true",
    "Float64Array=true,true,3,7,6,true",
    "BigInt64Array=true,true,3,7,6,true",
    "BigUint64Array=true,true,3,7,6,true",
    "DataView=true,true,2,1,8",
    "Number=true,42,41.0,object",
    "String=true,AB,2,b",
    "Boolean=true,true,object",
    "Date=true,2026,2026-01-02T00:00:00.000Z",
    "RegExp=true,a.,g,ab,2,3",
    "Function=true,42,2,function",
    "GeneratorFunction=true,4+5",
    "AsyncFunction=true,true",
    "AsyncGeneratorFunction=true,function",
    "Array=true,true,4,10,5,true",
];

#[test]
fn every_core_builtin_can_be_subclassed() {
    let out = run(&format!("{CORE}\nfor (const line of out) console.log(line);"));
    assert_eq!(out, CORE_EXPECTED);
}

#[cfg(not(feature = "wasm-lite"))]
#[test]
fn every_temporal_type_and_intl_service_can_be_subclassed() {
    let out = run(r#"
var out = [];
function t(label, f) { try { out.push(label + "=" + f()); } catch (e) { out.push(label + "!" + e); } }
t("Duration", () => { class D extends Temporal.Duration {} const d = new D(0, 0, 0, 0, 5); return [d instanceof D, d instanceof Temporal.Duration, d.hours, String(d)].join(","); });
t("PlainDate", () => { class D extends Temporal.PlainDate {} const d = new D(2026, 1, 2); return [d instanceof D, d.day, String(d)].join(","); });
t("PlainTime", () => { class D extends Temporal.PlainTime {} const d = new D(3, 4, 5); return [d instanceof D, d.minute, String(d)].join(","); });
t("PlainDateTime", () => { class D extends Temporal.PlainDateTime {} const d = new D(2026, 1, 2, 3, 4); return [d instanceof D, d.hour, String(d)].join(","); });
t("Instant", () => { class D extends Temporal.Instant {} const d = new D(0n); return [d instanceof D, String(d)].join(","); });
t("PlainYearMonth", () => { class D extends Temporal.PlainYearMonth {} const d = new D(2026, 3); return [d instanceof D, d.month, String(d)].join(","); });
t("PlainMonthDay", () => { class D extends Temporal.PlainMonthDay {} const d = new D(3, 9); return [d instanceof D, d.day, String(d)].join(","); });
t("ZonedDateTime", () => { class D extends Temporal.ZonedDateTime {} const d = new D(0n, "UTC"); return [d instanceof D, d.timeZoneId, String(d)].join(","); });
t("NumberFormat", () => { class D extends Intl.NumberFormat {} const d = new D("en"); return [d instanceof D, d.format(1234.5), d.resolvedOptions().locale].join(","); });
t("DateTimeFormat", () => { class D extends Intl.DateTimeFormat {} const d = new D("en", { timeZone: "UTC" }); return [d instanceof D, d.format(new Date(Date.UTC(2026, 0, 2)))].join(","); });
t("Collator", () => { class D extends Intl.Collator {} const d = new D("en"); return [d instanceof D, d.compare("a", "b")].join(","); });
t("PluralRules", () => { class D extends Intl.PluralRules {} const d = new D("en"); return [d instanceof D, d.select(1), d.select(2)].join(","); });
t("ListFormat", () => { class D extends Intl.ListFormat {} const d = new D("en"); return [d instanceof D, d.format(["a", "b", "c"])].join(","); });
t("RelativeTimeFormat", () => { class D extends Intl.RelativeTimeFormat {} const d = new D("en"); return [d instanceof D, d.format(-1, "day")].join(","); });
t("Segmenter", () => { class D extends Intl.Segmenter {} const d = new D("en"); return [d instanceof D, [...d.segment("ab")].length].join(","); });
t("Locale", () => { class D extends Intl.Locale {} const d = new D("en-US"); return [d instanceof D, d.language, d.region, String(d)].join(","); });
t("DisplayNames", () => { class D extends Intl.DisplayNames {} const d = new D("en", { type: "region" }); return [d instanceof D, typeof d.of("US")].join(","); });
t("DurationFormat", () => { class D extends Intl.DurationFormat {} const d = new D("en"); return [d instanceof D, d.format({ hours: 1 })].join(","); });
for (const line of out) console.log(line);
"#);
    assert_eq!(
        out,
        [
            "Duration=true,true,5,PT5H",
            "PlainDate=true,2,2026-01-02",
            "PlainTime=true,4,03:04:05",
            "PlainDateTime=true,3,2026-01-02T03:04:00",
            "Instant=true,1970-01-01T00:00:00Z",
            "PlainYearMonth=true,3,2026-03",
            "PlainMonthDay=true,9,03-09",
            "ZonedDateTime=true,UTC,1970-01-01T00:00:00+00:00[UTC]",
            "NumberFormat=true,1,234.5,en",
            "DateTimeFormat=true,1/2/2026",
            "Collator=true,-1",
            "PluralRules=true,one,other",
            "ListFormat=true,a, b, and c",
            "RelativeTimeFormat=true,1 day ago",
            "Segmenter=true,2",
            "Locale=true,en,US,en-US",
            "DisplayNames=true,string",
            "DurationFormat=true,1 hr",
        ]
    );
}
