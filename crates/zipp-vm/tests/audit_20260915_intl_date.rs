//! Regression coverage for the 15 September 2026 audit's Intl / Temporal /
//! Date track (R092, R169-R175, R179-R192, R294).
//!
//! * `TIERED` pins the engine-level fixes whose fast paths exist in more than
//!   one tier: a computed numeric index on a Map/Set is an ordinary [[Get]]
//!   (R174) while for-of, spread and destructuring still walk the entries; a
//!   string-keyed index written to `Array.prototype` feeds the indexed-prototype
//!   protector (R172); sealed and non-extensible arrays reject hole fills, stop
//!   a `length` shrink at a sealed element, and let `pop`/`shift`/`splice` run
//!   where only a delete can fail (R092); and a Date setter reads only its own
//!   parameters, with any non-finite field making the time value NaN
//!   (R170/R173). It runs in clean child processes under the default,
//!   interpreter and forced-JIT modes, each with hot loops.
//! * `INTL` checks formatting against node 24 (ICU 77): ja/zh month patterns
//!   that are numeric for a text request (R182), sub-minute offsets (R183),
//!   `longOffset` (R190), a fraction beside a minute (R294), numeric collation
//!   (R187), English ordinal plurals (R294), exact BigInt / numeric-string
//!   formatting (R188), percent in the decimal domain (R171/R192), rounding
//!   increments at any magnitude (R175) and `currencyDisplay` (R186).
//! * `TEMPORAL` has no node oracle (node 24 ships no Temporal), so its
//!   expectations are the proposal's: a non-ASCII scalar anywhere in an ISO
//!   string is a RangeError, never a process abort (R169); the ISO grammar has
//!   no whitespace and the zone annotation comes first (R191); date-only
//!   PlainDateTime strings take any annotation (R185); offset zone ids are
//!   exactly ±HH, ±HHMM or ±HH:MM, normalized to ±HH:MM (R184); out-of-range
//!   lunisolar years throw (R179); 12 months of a 13-month year stay P12M
//!   (R180); and `Duration.prototype.total` brackets calendar units in closed
//!   form (R189).

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

const TIERED: &str = r#"
var failures = [];
var checks = 0;
function eq(label, actual, expected) {
  if (!Object.is(actual, expected)) failures.push(label + ":" + String(actual));
  checks++;
}
function throwsType(label, op) {
  try { op(); failures.push(label + ":accepted"); }
  catch (e) { if (!(e instanceof TypeError)) failures.push(label + ":" + e.name); }
  checks++;
}

// R174: `coll[i]` on a Map/Set is an ordinary [[Get]] of the key "i", never
// the i-th entry; iteration, spread and destructuring still walk the entries.
(function () {
  var m = new Map([["a", 1], ["b", 2]]), s = new Set([10, 20]);
  eq("map[0]", m[0], undefined);
  eq("map[1]", m[1], undefined);
  eq("set[0]", s[0], undefined);
  eq("0-in-map", 0 in m, false);
  class MyMap extends Map {}
  eq("submap[0]", new MyMap([[1, 2]])[0], undefined);
  Map.prototype[0] = "proto0";
  eq("map-proto-index", m[0], "proto0");
  delete Map.prototype[0];
  m[0] = "own";
  eq("map-own-index", m[0], "own");
  s[0] = "own";
  eq("set-own-index", s[0], "own");
  var hot = new Set([1, 2]), defined = 0;
  for (var i = 0; i < 5000; i++) if (hot[i & 1] !== undefined) defined++;
  eq("set-index-hot", defined, 0);
  var mm = new Map([[1, "a"], [2, "b"], [3, "c"]]);
  mm.delete(2);
  var walked = "";
  for (var [k, v] of mm) walked += k + v;
  eq("map-for-of", walked, "1a3c");
  eq("map-spread", JSON.stringify([...mm]), '[[1,"a"],[3,"c"]]');
  var [[k1, v1], [k2, v2]] = mm;
  eq("map-destructure", k1 + v1 + k2 + v2, "1a3c");
  var ss = new Set([5, 6, 7]);
  ss.delete(6);
  var [a, b, c] = ss;
  eq("set-destructure", [a, b, c].join(), "5,7,");
  var [x, ...rest] = new Set([1, 2, 3]);
  eq("set-destructure-rest", x + ":" + rest.join(), "1:2,3");
  var total = 0;
  for (var j = 0; j < 3000; j++) { var [p, q] = new Set([j, -1]); total += p + q; }
  eq("set-destructure-hot", total, 3000 * 2999 / 2 - 3000);
  var setSum = 0;
  for (var r = 0; r < 3000; r++) for (var e of s) setSum += e;
  eq("set-for-of-hot", setSum, 90000);
})();

// R172: a string-keyed index written to Array.prototype runs the indexed-
// prototype bookkeeping, so holes see it (plain and "" + i concat keys).
(function () {
  var h = [0, , 2];
  var warm = 0;
  for (var i = 0; i < 5000; i++) if (h[1] === undefined) warm++;
  var key = "1";
  Array.prototype[key] = "P";
  eq("proto-hole-read", h[1], "P");
  eq("proto-hole-in", 1 in h, true);
  eq("proto-indexOf", h.indexOf("P"), 1);
  eq("proto-includes", h.includes("P"), true);
  eq("proto-json", JSON.stringify(h), '[0,"P",2]');
  eq("proto-length", Array.prototype.length, 2);
  delete Array.prototype[1];
  Array.prototype.length = 0;
  for (var n = 0; n < 3; n++) Array.prototype["" + n] = "C" + n;
  eq("proto-concat-key", [, , ,][1], "C1");
  eq("proto-concat-length", Array.prototype.length, 3);
  for (var n2 = 0; n2 < 3; n2++) delete Array.prototype[n2];
  Array.prototype.length = 0;
  eq("proto-warm", warm, 5000);
})();

// R092: array integrity levels.
(function () {
  var s = [1, 2, 3];
  Object.seal(s);
  s.length = 1;
  eq("sealed-length", s.length + JSON.stringify(s), "3[1,2,3]");
  (function () {
    "use strict";
    var s2 = Object.seal([1, 2, 3]);
    throwsType("sealed-length-strict", function () { s2.length = 1; });
    eq("sealed-length-strict-kept", s2.length, 3);
  })();
  var s3 = Object.seal([1, 2, , ,]);
  s3.length = 1;
  eq("sealed-length-trailing-holes", s3.length, 2);
  eq("sealed-reflect-set-length", Reflect.set(Object.seal([1, 2, 3]), "length", 1), false);
  var s4 = Object.seal([1, 2, 3]);
  eq("sealed-define-length", Reflect.defineProperty(s4, "length", { value: 1 }) + ":" + s4.length, "false:3");
  var h = Object.seal([1, , 3]);
  h[1] = 9;
  eq("sealed-hole-fill", h.hasOwnProperty(1) + JSON.stringify(h), "false[1,null,3]");
  var h2 = Object.preventExtensions([1, , 3]);
  h2[1] = 9;
  h2["1"] = 9;
  eq("nonextensible-hole-fill", h2.hasOwnProperty(1) + ":" + (1 in h2), "false:false");
  (function () {
    "use strict";
    var h3 = Object.preventExtensions([1, , 3]);
    throwsType("nonextensible-hole-fill-strict", function () { h3[1] = 5; });
    throwsType("nonextensible-hole-fill-strict-string", function () { h3["1"] = 5; });
  })();
  eq("nonextensible-define-hole", Reflect.defineProperty(Object.preventExtensions([1, , 3]), 1, { value: 2 }), false);
  var d = [1, , 3];
  Object.defineProperty(d, 1, { value: 2 });
  eq("define-hole-attributes", JSON.stringify(Object.getOwnPropertyDescriptor(d, 1)),
    '{"value":2,"writable":false,"enumerable":false,"configurable":false}');
  eq("frozen-define-element", Reflect.defineProperty(Object.freeze([1]), 0, { value: 2 }), false);
  eq("sealed-define-configurable", Reflect.defineProperty(Object.seal([1]), 0, { configurable: true }), false);
  var q = Object.preventExtensions([1, 2, 3]);
  eq("nonextensible-pop", q.pop() + ":" + q.length, "3:2");
  var q2 = Object.preventExtensions([1, 2, 3]);
  eq("nonextensible-shift", q2.shift() + JSON.stringify(q2), "1[2,3]");
  var q3 = Object.preventExtensions([1, 2, 3]);
  eq("nonextensible-splice", JSON.stringify(q3.splice(1, 1)) + JSON.stringify(q3), "[2][1,3]");
  var q4 = Object.seal([1, 2, ,]);
  eq("sealed-pop-trailing-hole", String(q4.pop()) + ":" + q4.length, "undefined:2");
  throwsType("sealed-pop", function () { Object.seal([1, 2, 3]).pop(); });
  var q5 = Object.seal([1, 2, 3]);
  throwsType("sealed-splice", function () { q5.splice(1, 1); });
  eq("sealed-splice-partial", JSON.stringify(q5), "[1,3,3]");
  throwsType("nonextensible-push", function () { Object.preventExtensions([1]).push(2); });
  throwsType("nonextensible-fill-holey", function () { Object.preventExtensions([1, , 3]).fill(7); });
  throwsType("frozen-pop", function () { Object.freeze([1]).pop(); });
  var big = [];
  for (var i = 0; i < 100; i++) big.push(i);
  Object.preventExtensions(big);
  var sum = 0;
  for (var i2 = 0; i2 < 50; i2++) sum += big.pop();
  eq("nonextensible-pop-hot", sum + ":" + big.length, "3725:50");
  var hot = Object.preventExtensions([1, , 3]), present = 0;
  for (var i3 = 0; i3 < 5000; i3++) { hot[1] = i3; if (1 in hot) present++; }
  eq("nonextensible-hole-fill-hot", present, 0);
  // OrdinarySet's BOOLEAN, not just the write: `Reflect.set` reported success
  // for every index write the integrity level made it drop.
  eq("reflect-sealed-hole", Reflect.set(Object.seal([1, , 3]), 1, 5), false);
  eq("reflect-sealed-present", Reflect.set(Object.seal([1, 2]), 1, 5), true);
  eq("reflect-sealed-new", Reflect.set(Object.seal([1, 2]), 5, 9), false);
  eq("reflect-nonextensible-hole", Reflect.set(Object.preventExtensions([1, , 3]), 1, 5), false);
  eq("reflect-nonextensible-new", Reflect.set(Object.preventExtensions([1, 2]), 9, 9), false);
  eq("reflect-nonextensible-present", Reflect.set(Object.preventExtensions([1, 2]), 0, 5), true);
  eq("reflect-frozen-present", Reflect.set(Object.freeze([1, 2]), 0, 5), false);
  eq("reflect-frozen-new", Reflect.set(Object.freeze([1, 2]), 7, 5), false);
  eq("reflect-plain-new", Reflect.set([1, 2], 5, 9), true);
  eq("reflect-plain-hole", Reflect.set([1, , 3], 1, 9), true);
  eq("reflect-sealed-named", Reflect.set(Object.seal([1]), "x", 1), false);
  var nw = [1, 2];
  Object.defineProperty(nw, "length", { writable: false });
  eq("reflect-nonwritable-length-past", Reflect.set(nw, 5, 1), false);
  eq("reflect-nonwritable-length-in", Reflect.set(nw, 0, 7) + ":" + nw[0], "true:7");
  eq("reflect-nonextensible-arguments", (function () {
    Object.preventExtensions(arguments);
    return Reflect.set(arguments, 5, 9);
  })(1), false);
  // The boolean must survive the tier transition too.
  var rtrue = 0, rfalse = 0;
  for (var i4 = 0; i4 < 5000; i4++) {
    if (Reflect.set(Object.seal([1, , 3]), 1, i4)) rtrue++; else rfalse++;
  }
  eq("reflect-sealed-hole-hot", rtrue + ":" + rfalse, "0:5000");
  // A FROZEN element is non-writable on every route to it, not just the
  // numeric-index one: a STRING key (Object.assign, a trap-less Proxy forward)
  // and an INHERITED element used to overwrite or shadow it.
  var fz = Object.freeze([1, 2]);
  throwsType("frozen-object-assign", function () { Object.assign(fz, { 0: 5 }); });
  eq("frozen-object-assign-kept", fz[0], 1);
  var fz2 = Object.freeze([1, 2]);
  new Proxy(fz2, {})["0"] = 5;
  eq("frozen-trapless-proxy", fz2[0], 1);
  eq("frozen-trapless-proxy-reflect", Reflect.set(new Proxy(Object.freeze([1, 2]), {}), 0, 5), false);
  eq("sealed-trapless-proxy-hole", Reflect.set(new Proxy(Object.seal([1, , 3]), {}), 1, 5), false);
  var inh = Object.create(Object.freeze([1, 2]));
  inh[0] = 9;
  eq("frozen-inherited", inh[0] + ":" + inh.hasOwnProperty(0), "1:false");
  (function () {
    "use strict";
    var inh2 = Object.create(Object.freeze([1, 2]));
    throwsType("frozen-inherited-strict", function () { inh2[0] = 9; });
  })();
  var deep = Object.create(Object.create(Object.freeze([1, 2])));
  deep[0] = 9;
  eq("frozen-inherited-deep", deep[0] + ":" + deep.hasOwnProperty(0), "1:false");
  // A WRITABLE inherited element is still merely shadowed, in every tier.
  var open = Object.create([1, 2]), shadowed = 0;
  for (var i5 = 0; i5 < 5000; i5++) { open[0] = i5; if (open.hasOwnProperty(0)) shadowed++; }
  eq("open-inherited-hot", shadowed + ":" + open[0], "5000:4999");
  var frz = Object.create(Object.freeze([1, 2])), owned = 0;
  for (var i6 = 0; i6 < 5000; i6++) { frz[0] = i6; if (frz.hasOwnProperty(0)) owned++; }
  eq("frozen-inherited-hot", owned + ":" + frz[0], "0:1");
  // Not frozen: a sealed or non-extensible array keeps writable elements, so an
  // inherited one is shadowed as usual.
  var sld = Object.create(Object.seal([1, 2]));
  sld[0] = 9;
  eq("sealed-inherited", sld[0] + ":" + sld.hasOwnProperty(0), "9:true");
  var nx = Object.create(Object.preventExtensions([1, 2]));
  nx[0] = 9;
  eq("nonextensible-inherited", nx[0] + ":" + nx.hasOwnProperty(0), "9:true");
  var fh = Object.create(Object.freeze([1, , 3]));
  fh[1] = 9;
  eq("frozen-inherited-hole", fh[1] + ":" + fh.hasOwnProperty(1), "9:true");
})();

// R170 / R173: a Date setter reads only its own parameters, and a non-finite
// field makes the time value NaN (as MakeDay/MakeTime do for the constructor).
(function () {
  var U = Date.UTC;
  eq("setUTCDate-extra", new Date(U(2020, 0, 1, 12)).setUTCDate(15, 10), U(2020, 0, 15, 12));
  eq("setUTCMonth-extra", new Date(U(2020, 0, 1, 12)).setUTCMonth(5, 15, 10, 30), U(2020, 5, 15, 12));
  eq("setUTCFullYear-extra", new Date(U(2020, 0, 1, 12)).setUTCFullYear(2021, 5, 15, 10), U(2021, 5, 15, 12));
  var cb = new Date(U(2020, 0, 1, 12));
  [5].forEach(cb.setUTCDate, cb);
  eq("setUTCDate-as-callback", cb.toISOString(), "2020-01-05T12:00:00.000Z");
  var coerced = 0;
  new Date(0).setUTCDate(2, { valueOf: function () { coerced++; return 3; } });
  eq("extra-argument-not-coerced", coerced, 0);
  eq("extra-NaN-ignored", new Date(0).setUTCDate(1, NaN), 0);
  eq("setUTCHours-four-params", new Date(0).setUTCHours(1, 2, 3, 4, 5), 3723004);
  eq("setUTCMonth-Infinity", new Date(0).setUTCMonth(Infinity), NaN);
  eq("setUTCDate-Infinity", new Date(0).setUTCDate(Infinity), NaN);
  eq("setUTCHours-minute-Infinity", new Date(0).setUTCHours(12, Infinity), NaN);
  eq("setMonth-Infinity", new Date(2020, 0, 15).setMonth(1 / 0), NaN);
  eq("setUTCFullYear-month-negative-Infinity", new Date(0).setUTCFullYear(2000, -Infinity), NaN);
  eq("setUTCMilliseconds-Infinity", new Date(0).setUTCMilliseconds(Infinity), NaN);
  eq("setUTCFullYear-huge", new Date(0).setUTCFullYear(50505469855533208), NaN);
  eq("setUTCHours-cancellation", new Date(0).setUTCHours(5124095576040, 0, 0, -18446744073709552000), 34447360);
  var acc = 0, hotDate = new Date(U(2020, 0, 1, 12));
  for (var t = 0; t < 5000; t++) acc += hotDate.setUTCDate(15, t % 24);
  eq("setUTCDate-extra-hot", acc, 5000 * U(2020, 0, 15, 12));
})();

console.log(failures.length ? "FAIL:" + failures.join(",") : "intl-date-tiered-ok:" + checks);
"#;

const INTL: &str = r#"
var failures = [];
var checks = 0;
function eq(label, actual, expected) {
  if (!Object.is(actual, expected)) failures.push(label + ":" + JSON.stringify(actual));
  checks++;
}
function parts(list) {
  return list.map(function (p) { return p.type + "=" + p.value; }).join("|");
}

// R182: a locale's numeric month pattern stays numeric for a text request
// (ja/zh write yMMMM as y年M月; widening M printed "2024年3月月").
var T = 1710054000000;
function dtf(locale, opts) {
  opts.timeZone = "UTC";
  return new Intl.DateTimeFormat(locale, opts).format(T);
}
eq("ja-year-month-long", dtf("ja", { year: "numeric", month: "long" }), "2024年3月");
eq("zh-year-month-long", dtf("zh", { year: "numeric", month: "long" }), "2024年3月");
eq("zh-CN-year-month-short", dtf("zh-CN", { year: "numeric", month: "short" }), "2024年3月");
eq("zh-TW-year-month-long", dtf("zh-TW", { year: "numeric", month: "long" }), "2024年3月");
eq("ja-month-long", dtf("ja", { month: "long" }), "3月");
eq("ja-month-long-day", dtf("ja", { month: "long", day: "numeric" }), "3月10日");
eq("zh-CN-month-long-day", dtf("zh-CN", { month: "long", day: "numeric" }), "3月10日");
eq("ja-era-year-month", dtf("ja", { era: "long", year: "numeric", month: "long" }), "西暦2024年3月");
eq("ja-date-style-long", dtf("ja", { dateStyle: "long" }), "2024年3月10日");
eq("en-year-month-long", dtf("en-US", { year: "numeric", month: "long" }), "March 2024");
eq("de-year-month-long", dtf("de", { year: "numeric", month: "long" }), "März 2024");
eq("ja-parts", parts(new Intl.DateTimeFormat("ja", { year: "numeric", month: "long", timeZone: "UTC" }).formatToParts(T)),
  "year=2024|literal=年|month=3|literal=月");
eq("ja-toLocaleDateString", new Date(T).toLocaleDateString("ja-JP", { year: "numeric", month: "long", timeZone: "UTC" }), "2024年3月");

// R183: sub-minute UTC offsets (LMT, Africa/Monrovia before 1972) keep their seconds.
function clock(tz, ms) {
  return new Intl.DateTimeFormat("en-US", { timeZone: tz, hour: "2-digit", minute: "2-digit", second: "2-digit", hourCycle: "h23" }).format(ms);
}
eq("monrovia-1938", clock("Africa/Monrovia", -1e12), "21:28:50");
eq("monrovia-1972", clock("Africa/Monrovia", Date.UTC(1972, 0, 1)), "23:15:30");
eq("kolkata-1900", clock("Asia/Kolkata", Date.UTC(1900, 0, 1)), "05:21:10");
eq("new-york-1850", clock("America/New_York", Date.UTC(1850, 0, 1)), "19:03:58");

// R190: longOffset is the long localized GMT format (+HH:mm), and a
// sub-minute offset prints its seconds in both offset styles.
function zone(tz, style, ms) {
  return new Intl.DateTimeFormat("en-US", { timeZone: tz, timeZoneName: style }).format(ms);
}
eq("new-york-longOffset", zone("America/New_York", "longOffset", 0), "12/31/1969, GMT-05:00");
eq("new-york-shortOffset", zone("America/New_York", "shortOffset", 0), "12/31/1969, GMT-5");
eq("kolkata-longOffset", zone("Asia/Kolkata", "longOffset", 0), "1/1/1970, GMT+05:30");
eq("kathmandu-longOffset", zone("Asia/Kathmandu", "longOffset", 0), "1/1/1970, GMT+05:30");
eq("monrovia-longOffset", zone("Africa/Monrovia", "longOffset", -1e12), "4/24/1938, GMT-00:44:30");
eq("monrovia-shortOffset", zone("Africa/Monrovia", "shortOffset", -1e12), "4/24/1938, GMT-0:44:30");

// R294 (4): a fraction beside a minute brings its second along.
var TF = 1710054789045;
eq("fraction-hour-minute", new Date(TF).toLocaleTimeString("en-US", { timeZone: "UTC", hour: "2-digit", minute: "2-digit", fractionalSecondDigits: 3 }), "07:13:09.045 AM");
eq("fraction-minute", new Date(TF).toLocaleTimeString("en-US", { timeZone: "UTC", minute: "2-digit", fractionalSecondDigits: 2 }), "13:09.04");
eq("fraction-de", new Intl.DateTimeFormat("de", { timeZone: "UTC", hour: "2-digit", minute: "2-digit", fractionalSecondDigits: 3 }).format(TF), "07:13:09,045");

// R187 / R294 (1): numeric collation.
var numeric = new Intl.Collator("en", { numeric: true });
eq("collator-numeric-sort", ["file10.txt", "file9.txt", "file100.txt"].sort(numeric.compare).join(), "file9.txt,file10.txt,file100.txt");
eq("collator-numeric-compare", numeric.compare("2", "10"), -1);
eq("collator-kn-extension", new Intl.Collator("en-u-kn").compare("2", "10"), -1);
eq("localeCompare-numeric", "2".localeCompare("10", undefined, { numeric: true }), -1);
eq("collator-default-compare", new Intl.Collator("en").compare("2", "10"), 1);
eq("collator-leading-zeros", ["a01", "a1", "a001", "a2", "a10", "b1", "A1"].sort(numeric.compare).join(), "a01,a1,a001,A1,a2,a10,b1");
eq("collator-leading-zeros-equal-base", new Intl.Collator("en", { numeric: true, sensitivity: "base" }).compare("a01", "a1"), 0);
eq("collator-arabic-indic", ["١٠", "٩", "10", "9"].sort(numeric.compare).join(), "٩,9,١٠,10");

// R294 (2): English ordinal plural rules.
var ordinal = new Intl.PluralRules("en", { type: "ordinal" });
eq("ordinal-select", [1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 101, 111, 112, 0].map(function (n) { return ordinal.select(n); }).join(),
  "one,two,few,other,other,other,other,one,two,few,one,other,other,other");
eq("ordinal-categories", ordinal.resolvedOptions().pluralCategories.join(), "one,two,few,other");
eq("cardinal-select", [1, 2, 0, -1].map(function (n) { return new Intl.PluralRules("en").select(n); }).join(), "one,other,other,one");
eq("cardinal-fraction-digits", new Intl.PluralRules("en", { minimumFractionDigits: 1 }).select(1), "other");

// R188 / R294 (3): BigInt and numeric strings format exactly.
var nf = new Intl.NumberFormat("en-US");
eq("bigint-format", nf.format(1234567890123456789n), "1,234,567,890,123,456,789");
eq("bigint-format-2^64", nf.format(2n ** 64n), "18,446,744,073,709,551,616");
eq("bigint-negative", nf.format(-9007199254740993n), "-9,007,199,254,740,993");
eq("bigint-toLocaleString", (2n ** 64n).toLocaleString("en-US"), "18,446,744,073,709,551,616");
eq("string-format", nf.format("1234567890123456789012345.678"), "1,234,567,890,123,456,789,012,345.678");
eq("string-format-20-digits", nf.format("12345678901234567890"), "12,345,678,901,234,567,890");
eq("bigint-parts", nf.formatToParts(1234567890123456789n).map(function (p) { return p.value; }).join(""), "1,234,567,890,123,456,789");
eq("bigint-range", nf.formatRange(1n, 12345678901234567890123n), "1–12,345,678,901,234,567,890,123");
eq("bigint-significant", new Intl.NumberFormat("en-US", { maximumSignificantDigits: 3 }).format(12345678901234567890123n), "12,300,000,000,000,000,000,000");
eq("bigint-currency", new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" }).format(12345678901234567890123n), "$12,345,678,901,234,567,890,123.00");
eq("string-hex", nf.format(" 0x1F "), "31");
eq("string-exponent", nf.format("1e3"), "1,000");
eq("string-negative-zero", nf.format("-0"), "-0");
eq("string-tiny", new Intl.NumberFormat("en-US", { maximumFractionDigits: 20 }).format("0.000000000000000000001234"), "0");
eq("string-tie", new Intl.NumberFormat("en-US", { maximumFractionDigits: 3 }).format("1.0005"), "1.001");

// R171 / R192: percent is 100 × the decimal value, NaN keeps its sign, and a
// finite value never overflows to infinity.
var pct = new Intl.NumberFormat("en-US", { style: "percent" });
eq("percent-ties", [0.145, 0.285, 0.575, 1.005].map(function (n) { return pct.format(n); }).join(" "), "15% 29% 58% 101%");
eq("percent-tiny", new Intl.NumberFormat("en-US", { style: "percent", maximumFractionDigits: 20 }).format(1e-7), "0.00001%");
eq("percent-NaN", pct.format(NaN), "NaN%");
eq("percent-NaN-always", new Intl.NumberFormat("en-US", { style: "percent", signDisplay: "always" }).format(NaN), "+NaN%");
eq("percent-NaN-parts", parts(pct.formatToParts(NaN)), "nan=NaN|percentSign=%");
eq("percent-1e307-finite", pct.format(1e307).indexOf("∞"), -1);
eq("percent-1e307-length", pct.format(1e307).replace(/,/g, "").length, 311);
eq("compact-rounding-priority", new Intl.NumberFormat("en-US", { notation: "compact" }).resolvedOptions().roundingPriority, "morePrecision");

// R175: roundingIncrement at any magnitude.
eq("increment-1e19", new Intl.NumberFormat("en-US", { roundingIncrement: 25, minimumFractionDigits: 20, maximumFractionDigits: 20 }).format(1e19),
  "10,000,000,000,000,000,000.00000000000000000000");
eq("increment-2e38", new Intl.NumberFormat("en-US", { roundingIncrement: 5, minimumFractionDigits: 2, maximumFractionDigits: 2 }).format(2e38),
  "200,000,000,000,000,000,000,000,000,000,000,000,000.00");
eq("increment-small", new Intl.NumberFormat("en-US", { roundingIncrement: 5, minimumFractionDigits: 2, maximumFractionDigits: 2 }).format(12.34), "12.35");

// R186: currencyDisplay code / name.
function cur(code, display, n) {
  return new Intl.NumberFormat("en-US", { style: "currency", currency: code, currencyDisplay: display }).format(n);
}
eq("currency-code", cur("USD", "code", 1234.5), "USD\u00a01,234.50");
eq("currency-code-jpy", cur("JPY", "code", 1234.5), "JPY\u00a01,235");
eq("currency-code-negative", cur("USD", "code", -1234.5), "-USD\u00a01,234.50");
eq("currency-name", cur("USD", "name", 1234.5), "1,234.50 US dollars");
eq("currency-name-euro", cur("EUR", "name", 1234.5), "1,234.50 euros");
eq("currency-name-one", cur("USD", "name", 1), "1.00 US dollars");
eq("currency-symbol", cur("USD", "symbol", 1), "$1.00");
eq("currency-code-parts", parts(new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", currencyDisplay: "code" }).formatToParts(-1)),
  "minusSign=-|currency=USD|literal=\u00a0|integer=1|decimal=.|fraction=00");
eq("currency-name-parts", parts(new Intl.NumberFormat("en-US", { style: "currency", currency: "USD", currencyDisplay: "name" }).formatToParts(-1)),
  "minusSign=-|integer=1|decimal=.|fraction=00|literal= |currency=US dollars");
eq("currency-code-toLocaleString", (1).toLocaleString("en-US", { style: "currency", currency: "USD", currencyDisplay: "code" }), "USD\u00a01.00");

console.log(failures.length ? "FAIL:" + failures.join(",") : "intl-ok:" + checks);
"#;

const TEMPORAL: &str = r#"
var failures = [];
var checks = 0;
function eq(label, actual, expected) {
  if (!Object.is(actual, expected)) failures.push(label + ":" + String(actual));
  checks++;
}
function rangeError(label, op) {
  try { var r = op(); failures.push(label + ":accepted:" + String(r)); }
  catch (e) { if (!(e instanceof RangeError)) failures.push(label + ":" + (e && e.name)); }
  checks++;
}
var PD = Temporal.PlainDate, PDT = Temporal.PlainDateTime, D = Temporal.Duration;

// R169: a non-ASCII scalar at ANY offset of an ISO string is a RangeError. The
// field parsers sliced at byte offsets and aborted the process instead.
var seeds = [
  [PD, "2020-01-01"], [PD, "+002020-01-01T12:00"], [PD, "20200101"],
  [PDT, "2020-01-01T12:34:56.789+01:00[UTC][u-ca=iso8601]"],
  [Temporal.PlainTime, "12:34:56.789"], [Temporal.PlainTime, "1234567890"],
  [Temporal.PlainYearMonth, "2020-01"], [Temporal.PlainYearMonth, "202001"],
  [Temporal.PlainMonthDay, "--01-01"], [Temporal.PlainMonthDay, "0101"],
  [Temporal.Instant, "2020-01-01T00:00:00Z"],
  [Temporal.ZonedDateTime, "2020-01-01T00:00+01:00[+01:00]"],
  [D, "P1Y2M3DT4H5M6.5S"]
];
var inserts = ["é", "😀", "\u00a0"];
var mutated = 0;
seeds.forEach(function (seed) {
  var type = seed[0], s = seed[1];
  for (var i = 0; i <= s.length; i++) {
    inserts.forEach(function (c) {
      var variants = [s.slice(0, i) + c + s.slice(i)];
      if (i < s.length) variants.push(s.slice(0, i) + c + s.slice(i + 1));
      variants.forEach(function (v) {
        try { type.from(v); failures.push("non-ascii-accepted:" + JSON.stringify(v)); }
        catch (e) { if (!(e instanceof RangeError)) failures.push("non-ascii:" + JSON.stringify(v) + ":" + e.name); }
        mutated++;
      });
    });
  }
});
eq("non-ascii-cases", mutated > 1000, true);
rangeError("non-ascii-calendar-field", function () { return PD.from({ year: 2020, month: 1, day: 1, calendar: "2020-1é" }); });
rangeError("non-ascii-compare", function () { return PD.compare("2020-01-1é", "2020-01-01"); });
rangeError("non-ascii-offset-zone", function () { return Temporal.Now.zonedDateTimeISO("+0é1"); });
rangeError("non-ascii-offset-zone-intl", function () { return new Intl.DateTimeFormat("en", { timeZone: "+0é1" }); });

// R191: the ISO grammar has no whitespace, a zone annotation is a non-empty
// identifier that comes first, and nothing may trail the annotations.
[" 2020-01-01", "2020-01-01\n", "\u00a02020-01-01", "2020-01-01[]", "2020-01-01[!]",
 "2020-01-01[u-ca=iso8601][UTC]", "2020-01-01[UTC][UTC]", "2020-01-01[é]", "2020-01-01[u-ca=]",
 "2020-01-01 [UTC]", "2020-01-01[UTC]x"].forEach(function (s) {
  rangeError("grammar-PlainDate:" + JSON.stringify(s), function () { return PD.from(s).toString(); });
});
rangeError("grammar-Duration", function () { return D.from(" P1D"); });
rangeError("grammar-PlainTime", function () { return Temporal.PlainTime.from(" 12:00"); });
rangeError("grammar-Instant", function () { return Temporal.Instant.from("2020-01-01T00:00Z "); });
rangeError("grammar-ZonedDateTime-trailing", function () { return Temporal.ZonedDateTime.from("2020-01-01T00:00[UTC] "); });
rangeError("grammar-ZonedDateTime-after-Z", function () { return Temporal.ZonedDateTime.from("2020-01-01T00:00Zé[UTC]"); });
rangeError("grammar-ZonedDateTime-empty-time", function () { return Temporal.ZonedDateTime.from("2020-01-01T[UTC]"); });
rangeError("grammar-relativeTo", function () { return D.from({ days: 1 }).round({ largestUnit: "months", relativeTo: "2020-01-01 [UTC]" }); });
rangeError("grammar-calendar-id", function () { return PD.from("2020-01-01").withCalendar(" gregory"); });
rangeError("grammar-calendar-junk-string", function () { return PD.from("2020-01-01").withCalendar("é2020-01-01[u-ca=hebrew]"); });
rangeError("grammar-bag-calendar", function () { return PD.from({ year: 2020, month: 1, day: 1, calendar: " iso8601" }); });
rangeError("grammar-zone-trailing", function () { return Temporal.ZonedDateTime.from({ year: 2020, month: 1, day: 1, timeZone: "2020-01-01T00:00+01:00[-02:00]x" }); });
eq("grammar-valid-annotations", PD.from("2020-01-01[UTC][u-ca=gregory]").toString(), "2020-01-01[u-ca=gregory]");
eq("grammar-unknown-key", PD.from("2020-01-01[Etc/GMT+5][foo=bar-1]").toString(), "2020-01-01");
eq("grammar-critical-calendar", PD.from("2020-01-01[!u-ca=iso8601]").toString(), "2020-01-01");
eq("grammar-calendar-string", PD.from("2020-01-01").withCalendar("2020-01-01[u-ca=hebrew]").calendarId, "hebrew");
eq("grammar-space-separator", PDT.from("2020-01-01 12:00").toString(), "2020-01-01T12:00:00");
eq("grammar-zone-from-string", Temporal.ZonedDateTime.from({ year: 2020, month: 1, day: 1, timeZone: "2020-01-01T00:00+01:00[-02:00]" }).timeZoneId, "-02:00");

// R185: a date-only PlainDateTime string may carry any annotation, including
// ones with a 't' in them.
eq("pdt-annotation-UTC", PDT.from("2020-01-01[UTC]").toString(), "2020-01-01T00:00:00");
eq("pdt-annotation-calendar", PDT.from("2020-01-01[u-ca=ethiopic]").toString(), "2020-01-01T00:00:00[u-ca=ethiopic]");
eq("pdt-annotation-toronto", PDT.from("2020-01-01[America/Toronto]").toString(), "2020-01-01T00:00:00");
eq("pdt-annotation-basic", PDT.from("20200101[Etc/UTC]").toString(), "2020-01-01T00:00:00");
eq("pdt-compare", PDT.compare("2020-01-01[u-ca=islamic-tbla]", "2020-01-01"), 0);
eq("pdt-equals", PDT.from("2020-01-01T00:00").equals("2020-01-01[UTC]"), true);
eq("pdt-since", PDT.from("2020-01-02").since("2020-01-01[Etc/UTC]").toString(), "P1D");

// R184: offset time zone identifiers are exactly ±HH, ±HHMM or ±HH:MM, and
// are normalized to ±HH:MM.
var instant = Temporal.Instant.from("2020-01-01T00:00Z");
["+1:5", "+001:00", "+-01:00", "+01:-05", "+01:00:00", " UTC", "UTC\n", "+24:00"].forEach(function (tz) {
  rangeError("offset-zone:" + JSON.stringify(tz), function () { return instant.toZonedDateTimeISO(tz); });
});
rangeError("offset-zone-bracket", function () { return Temporal.ZonedDateTime.from("2020-01-01T00:00[+1:5]"); });
rangeError("offset-zone-bag", function () { return Temporal.ZonedDateTime.from({ year: 2020, month: 1, day: 1, timeZone: "+1:5" }); });
rangeError("offset-zone-with", function () { return new Temporal.ZonedDateTime(0n, "UTC").withTimeZone("+1:5"); });
rangeError("offset-zone-bag-space", function () { return Temporal.ZonedDateTime.from({ year: 2020, month: 1, day: 1, timeZone: " UTC" }); });
eq("offset-zone-basic", instant.toZonedDateTimeISO("+0100").timeZoneId, "+01:00");
eq("offset-zone-minus-zero", instant.toZonedDateTimeISO("-00:00").timeZoneId, "+00:00");
eq("offset-zone-hour-only", instant.toZonedDateTimeISO("+01").timeZoneId, "+01:00");
eq("offset-zone-offset", instant.toZonedDateTimeISO("-0530").offset, "-05:30");
eq("offset-zone-ctor", new Temporal.ZonedDateTime(0n, "+0100").timeZoneId, "+01:00");
eq("offset-zone-ctor-minus-zero", new Temporal.ZonedDateTime(0n, "-00:00").timeZoneId, "+00:00");
eq("offset-zone-annotation", Temporal.ZonedDateTime.from("1970-01-01T00:00+01:00[+0100]").timeZoneId, "+01:00");
eq("offset-zone-now", Temporal.Now.zonedDateTimeISO("+0100").timeZoneId, "+01:00");
eq("named-zone-case", Temporal.ZonedDateTime.from({ year: 2020, month: 1, day: 1, timeZone: "etc/gmt+5" }).timeZoneId, "Etc/GMT+5");

// R179: lunisolar years/months past the astronomy are out of range, not
// folded back onto a valid date.
var chinese = PD.from({ year: 2020, month: 1, day: 1, calendar: "chinese" });
eq("chinese-in-range", chinese.toString(), "2020-01-25[u-ca=chinese]");
[1e12, 2 ** 53, -1e12, 300000].forEach(function (y) {
  rangeError("chinese-year:" + y, function () { return PD.from({ calendar: "chinese", year: y, month: 1, day: 1 }).toString(); });
});
rangeError("dangi-year", function () { return PD.from({ calendar: "dangi", year: 1e12, month: 1, day: 1 }).toString(); });
rangeError("chinese-monthCode-year", function () { return PD.from({ calendar: "chinese", year: 1e12, monthCode: "M01", day: 1 }).toString(); });
rangeError("chinese-with-year", function () { return chinese.with({ year: 1e12 }).toString(); });
rangeError("chinese-add-months", function () { return chinese.add({ months: 2 ** 32 - 1 }).toString(); });
rangeError("chinese-subtract-months", function () { return chinese.add({ months: -(2 ** 32 - 1) }).toString(); });
rangeError("chinese-ym-add", function () { return Temporal.PlainYearMonth.from({ calendar: "chinese", year: 2020, month: 1 }).add({ months: 2 ** 32 - 1 }).toString(); });
rangeError("chinese-pdt-add", function () { return chinese.toPlainDateTime().add({ months: 2 ** 32 - 1 }).toString(); });
rangeError("chinese-zdt-add", function () { return chinese.toZonedDateTime("UTC").add({ months: 2 ** 32 - 1 }).toString(); });
rangeError("chinese-round", function () { return D.from({ months: 2 ** 32 - 1 }).round({ largestUnit: "years", relativeTo: chinese }).toString(); });
rangeError("chinese-compare", function () { return D.compare({ months: 2 ** 32 - 1 }, { months: 1 }, { relativeTo: chinese }); });
rangeError("chinese-total", function () { return D.from({ months: 2 ** 32 - 1 }).total({ unit: "months", relativeTo: chinese }); });
eq("chinese-far-in-range", PD.from({ calendar: "chinese", year: 275760, month: 1, day: 1 }).toString(), "+275759-02-23[u-ca=chinese]");

// R180: 12 months of a 13-month year are P12M, not P1Y (hebrew, chinese).
var ha = PD.from({ calendar: "hebrew", year: 5784, monthCode: "M01", day: 1 });
var hb = ha.add({ months: 12, days: 9 });
var monthsTrunc = { largestUnit: "years", smallestUnit: "months", roundingMode: "trunc" };
eq("hebrew-until-exact", ha.until(hb, { largestUnit: "years" }).toString(), "P12M9D");
eq("hebrew-until-trunc", ha.until(hb, monthsTrunc).toString(), "P12M");
eq("hebrew-pdt-until-trunc", ha.toPlainDateTime().until(hb.toPlainDateTime(), monthsTrunc).toString(), "P12M");
eq("hebrew-zdt-until-trunc", ha.toZonedDateTime("UTC").until(hb.toZonedDateTime("UTC"), monthsTrunc).toString(), "P12M");
eq("hebrew-round", D.from({ months: 12, days: 9 }).round({ largestUnit: "years", smallestUnit: "months", relativeTo: ha }).toString(), "P12M");
eq("hebrew-since", hb.since(ha, monthsTrunc).toString(), "P12M");
eq("hebrew-since-negative", ha.since(hb, monthsTrunc).toString(), "-P12M");
eq("hebrew-until-ceil", ha.until(hb, { largestUnit: "years", smallestUnit: "months", roundingMode: "ceil" }).toString(), "P1Y");
eq("hebrew-until-13-months", ha.until(ha.add({ months: 13 }), { largestUnit: "years", smallestUnit: "months" }).toString(), "P1Y");
eq("hebrew-until-12-months-20-days", ha.until(ha.add({ months: 12, days: 20 }), { largestUnit: "years", smallestUnit: "months" }).toString(), "P12M");
var cc = PD.from("2024-12-01[u-ca=chinese]");
eq("chinese-until-trunc", cc.until(cc.add({ months: 12, days: 5 }), monthsTrunc).toString(), "P12M");
eq("iso-until-bubble", PD.from("2020-01-01").until(PD.from("2021-12-20"), { largestUnit: "years", smallestUnit: "months", roundingMode: "halfExpand" }).toString(), "P2Y");
eq("iso-until-no-bubble", PD.from("2020-01-01").until(PD.from("2021-12-17"), { largestUnit: "years", smallestUnit: "months", roundingMode: "trunc" }).toString(), "P1Y11M");
eq("iso-until-negative", PD.from("2021-12-20").until(PD.from("2020-01-01"), { largestUnit: "years", smallestUnit: "months", roundingMode: "halfExpand" }).toString(), "-P2Y");

// R189: Duration.prototype.total brackets a calendar unit in closed form, so
// a long span answers (it used to walk unit by unit into a 2M-step cap).
eq("total-90M-days-months", D.from({ days: 90000000 }).total({ unit: "month", relativeTo: "1900-01-01" }), 2956939 + 19 / 31);
eq("total-2000001-months", new D(0, 2000001).total({ unit: "month", relativeTo: "-100000-01-01" }), 2000001);
eq("total-chinese-months", new D(0, 1900000).total({ unit: "month", relativeTo: PD.from({ calendar: "chinese", year: -100000, monthCode: "M01", day: 1 }) }), 1900000);
eq("total-negative", D.from({ days: -90000000 }).total({ unit: "month", relativeTo: "2020-01-31" }) < -2956939, true);
eq("total-end-of-month", D.from({ days: 335 }).total({ unit: "month", relativeTo: "2023-05-31" }), 11);
eq("total-end-of-month-negative", D.from({ months: -11 }).total({ unit: "month", relativeTo: "2024-04-30" }), -11);
eq("total-fraction", D.from({ days: 1000 }).total({ unit: "month", relativeTo: "2020-01-31" }), 32.87096774193548);
eq("total-years", D.from({ days: 90000000 }).total({ unit: "year", relativeTo: "1900-01-01" }), 246411.63287671233);

console.log(failures.length ? "FAIL:" + failures.join(",") : "temporal-ok:" + checks);
"#;

#[test]
fn audit_20260915_intl_date_tiered_child() {
    if std::env::var_os("ZIPP_AUDIT_INTL_DATE_CHILD").is_none() {
        return;
    }
    assert_eq!(run_ok(TIERED), ["intl-date-tiered-ok:95"]);
}

#[test]
fn audit_20260915_intl_date_tiers_match() {
    if std::env::var_os("ZIPP_AUDIT_INTL_DATE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("default", None),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args([
            "--exact",
            "audit_20260915_intl_date_tiered_child",
            "--nocapture",
        ])
        // An inherited setting must not decide what a mode tests.
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env("ZIPP_AUDIT_INTL_DATE_CHILD", "1");
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        // `--exact` filtering everything out would also exit 0.
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "tiered/{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            stdout,
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn audit_20260915_intl_formatting() {
    assert_eq!(run_ok(INTL), ["intl-ok:74"]);
}

#[test]
fn audit_20260915_temporal_strings_and_calendars() {
    assert_eq!(run_ok(TEMPORAL), ["temporal-ok:100"]);
}
