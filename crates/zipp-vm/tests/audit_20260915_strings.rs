//! Regression coverage for the 15 September 2026 audit's strings-and-JSON
//! track.
//!
//! * `Number.prototype.toString(radix)` printed only a saturated integer part:
//!   `(0.5).toString(2)` was `"0"`, `(-0.5).toString(2)` lost its sign, every
//!   magnitude at or above 2^64 printed `ffff…`, and the ID idiom
//!   `Math.random().toString(36).slice(2)` was always `""`.
//! * String built-ins read the receiver and arguments through a lossy Rust
//!   `String`, so a lone surrogate became U+FFFD: a lone-surrogate needle
//!   matched a real U+FFFD, and replace/normalize/locale case mapping
//!   dropped surrogates. An `Intl.Segmenter` iteration step re-segmented
//!   the whole input from 0.
//! * The same conversion in the builders that join text: `Array.prototype
//!   .join`/`toString`/`toLocaleString` (so `s.split('').join('')` and
//!   `split('').reverse().join('')` corrupted emoji text), array spread of a
//!   string, `String.raw`, a regexp replacement template and every
//!   GetSubstitution selector (`$&`, `` $` ``, `$'`, `$N`, `$<name>`) —
//!   through the fast path and through the observable `@@replace` protocol.
//! * `String.prototype.split(re)` took its internal fast path without proving
//!   that `RegExp.prototype[@@split]` was still the intrinsic, so an override
//!   was honoured or ignored depending on the call shape.
//! * `Math.hypot` squared without scaling (Infinity for `hypot(1e300, 1e300)`,
//!   0 for `hypot(3e-200, 4e-200)`), and trim/`Number`/`BigInt` used Rust's
//!   whitespace rules instead of StrWhiteSpace (U+0085 stripped, U+FEFF
//!   rejected by `BigInt`).
//! * Native builders without a size preflight (`split`'s part count,
//!   `toLocaleUpperCase`, `padStart`) built past the string and array caps and
//!   trapped the WebAssembly build.
//! * `JSON.stringify` serialized constructors, classes and callable Proxies as
//!   `{}`, called `toJSON` on string and symbol primitives, lost a String
//!   wrapper's lone surrogates, cut a string `space` at ten code points, and
//!   (with `JSON.parse` and its reviver) recursed without bound in the
//!   ordinary profile.
//! * Smaller numeric faults: exact ties between two shortest digit strings
//!   went up instead of to the even one, BigInt `**` truncated a 2^64
//!   exponent, and legacy `Date.parse` wrapped absurd years and offsets back
//!   into range.
//!
//! Outputs that node computes identically are pinned as node's; the string
//! operations are checked against reference implementations over code units
//! written in the probe itself.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// Numeric conversions and operators, cold and in loops that tier up. Run in
/// child processes per mode.
const TIER_PROBE: &str = r##"
var lines = [];
function log(label, value) { lines.push(label + "=" + value); }
// Number.prototype.toString(radix): fraction, sign, magnitudes past 2^64.
log("radix-frac", [(0.5).toString(2), (255.5).toString(16), (2.5).toString(16), (0.1).toString(3), (0.1).toString(36)].join(","));
log("radix-sign", [(-0.75).toString(2), (-0.5).toString(2), (-255).toString(16), (-1e21).toString(16)].join(","));
log("radix-big", [(1e21).toString(16), (2 ** 64).toString(16), (2 ** 64).toString(2).length, (1e21).toString(36), (-1e300).toString(36).length, Number.MAX_VALUE.toString(36).length].join(","));
log("radix-tiny", [(5e-324).toString(2).length, (1e-7).toString(16), (2 ** -1074).toString(36).slice(0, 12)].join(","));
log("radix-special", [NaN.toString(2), Infinity.toString(16), (-Infinity).toString(36), (-0).toString(2), (0).toString(7)].join(","));
log("radix-id", Math.random().toString(36).slice(2).length > 0);
var ids = new Set();
for (var i = 0; i < 200; i++) ids.add(Math.random().toString(36).substring(2, 9));
log("radix-ids", ids.size > 190);
var sum = 0;
for (var i = 0; i < 3000; i++) sum += (i + 0.5).toString(2).length;
log("radix-hot", sum);
// Ties between equally short digit strings go to the even one.
log("ties", [String(9007199254741024 / 15), String(-600479950316035.25), `${1286742750677258.25}`, JSON.stringify(600479950316068.25), (600479950316068.25).toExponential(), String(0.3), String(1.5), String(123.456)].join(","));
var t = "";
for (var i = 0; i < 3000; i++) t = String(600479950316068.25 + 0 * i);
log("ties-hot", t);
// BigInt ** with a huge exponent is a RangeError, not the exponent mod 2^64.
var e = 2n ** 64n;
function bigpow(f) { try { return String(f()); } catch (err) { return err.name; } }
log("bigpow", [bigpow(function () { return 2n ** e; }), bigpow(function () { return 3n ** (e + 2n); }), bigpow(function () { var x = 5n; x **= (e + 2n); return x; }), bigpow(function () { return 1n ** e; }), bigpow(function () { return (-1n) ** (e + 1n); }), bigpow(function () { return 0n ** e; }), bigpow(function () { return 2n ** 100n; })].join(","));
var br = "";
for (var i = 0; i < 3000; i++) br = bigpow(function () { return 2n ** e; });
log("bigpow-hot", br);
// The string builtins that build a result keep every code unit.
function hex(s) { var o = []; for (var i = 0; i < s.length; i++) o.push(s.charCodeAt(i).toString(16)); return o.join("."); }
var L = "\uD83D", T = "\uDE00", W = "a" + L + "b" + T + "c";
// join / toString / spread keep every code unit: the everyday round trip.
log("join-pair", hex([L, T].join("")));
log("join-round", [hex(W.split("").join("")), hex(W.split("").reverse().join("")), hex([...W].join(""))].join(","));
log("join-sep", [hex(["x", "y"].join(L)), hex([L, T].toString()), hex([[L], [T]].join(""))].join(","));
log("join-nullish", [hex([null, L, undefined].join("-")), hex(new Array(3).join(L))].join(","));
log("join-arraylike", hex(Array.prototype.join.call({ length: 2, 0: L, 1: T }, "")));
log("join-tolocale", hex([{ toLocaleString: function () { return L; } }, T].toLocaleString()));
log("join-typedarray", [hex(new Uint8Array([1, 2]).join(L)), new Uint8Array([1, 2]).toString()].join(","));
var jh = "";
for (var i = 0; i < 3000; i++) jh = [L, T].join("");
log("join-hot", hex(jh));
// String.raw, the builtin and the tag form.
log("raw", [hex(String.raw`a${L}b${T}`), hex(String.raw({ raw: ["x" + L, T + "y"] }, "-"))].join(","));
// A regexp replacement template and GetSubstitution are exact.
log("re-tmpl", [hex("abc".replace(/b/, L)), hex("aéc".replace(/é/, L + T))].join(","));
log("re-sub", [hex(("a" + L + "b").replace(/./, "<$&>")), hex(("a" + L + "b").replace(/b/, "[$`]")), hex(("b" + L + "c").replace(/b/, "[$']"))].join(","));
log("re-group", [hex(("a" + L + "b").replace(/(a)(.)/, "[$1|$2]")), hex(("a" + L + "b").replace(/(?<x>.)b/, "[$<x>]")), hex(("a" + L + "b").replace(/(z)?(.)b/, "[$1|$2]"))].join(","));
log("re-protocol", [hex(RegExp.prototype[Symbol.replace].call(/./, "a" + L + "b", "<$&>")), hex(RegExp.prototype[Symbol.replace].call(/(?<x>.)b/, "a" + L + "b", "[$<x>]"))].join(","));
var rh = "";
for (var i = 0; i < 3000; i++) rh = ("a" + L + "b").replace(/b/, "[$`]");
log("re-hot", hex(rh));
// A user @@split overrides the RegExp fast path, whatever the call shape.
function split3() {
  var own = /a/;
  own[Symbol.split] = function () { return ["own"]; };
  var a = "xay".split(own).join("|");
  var orig = RegExp.prototype[Symbol.split];
  RegExp.prototype[Symbol.split] = function () { return ["proto"]; };
  var b = "xay".split(/a/).join("|");
  var c = String.prototype.split.call("xay", /a/).join("|");
  class Sub extends RegExp { [Symbol.split]() { return ["sub"]; } }
  var d = "xay".split(new Sub("a")).join("|");
  RegExp.prototype[Symbol.split] = orig;
  var e = "xay".split(/a/).join("|");
  return [a, b, c, d, e].join(",");
}
log("sym-split", split3());
var sh = "";
for (var i = 0; i < 3000; i++) sh = "xay".split(/a/).join("|");
log("split-hot", sh);
// Math.hypot scales: a representable result is not lost.
log("hypot", [Math.hypot(1e300, 1e300), Math.hypot(3e300, 4e300), Math.hypot(3e-200, 4e-200), Math.hypot(1e-200, 1e-200), Math.hypot(-1e-300)].join(","));
log("hypot-edge", [Math.hypot(), Math.hypot(-0), Math.hypot(0, -0), Math.hypot(NaN, Infinity), Math.hypot(Infinity, NaN), Math.hypot(NaN, 1), Math.hypot(3, 4), Math.hypot(...[3e300, 4e300])].join(","));
var hh = 0;
for (var i = 0; i < 3000; i++) hh = Math.hypot(3e300, 4e300);
log("hypot-hot", hh);
// StrWhiteSpace: U+0085 is not whitespace, U+FEFF is.
log("trim-ws", ["a".trim().length, "".trim().length, "﻿a﻿".trim().length, " \t\na ".trim().length].join(","));
log("num-ws", [Number("1"), Number(""), Number("﻿1"), Number("﻿"), Number(" 2 ")].join(","));
function bi(s) { try { return String(BigInt(s)); } catch (e) { return e.name; } }
log("bigint-ws", [bi("﻿1"), bi("1"), bi(""), bi(" 3 "), bi("")].join(","));
log("bigint-cmp", [1n == "﻿1", 1n == "1", 0n == "", 1n < "2"].join(","));
var wh = 0;
for (var i = 0; i < 3000; i++) wh = Number("1");
log("num-ws-hot", String(wh));
console.log(lines.join(";"));
"##;

/// node v24's output for [`TIER_PROBE`].
const TIER_EXPECTED: [&str; 36] = [
    "radix-frac=0.1,ff.8,2.8,0.0022002200220022002200220022002201,0.3lllllllllm",
    "radix-sign=-0.11,-0.1,-ff,-3635c9adc5dea00000",
    "radix-big=3635c9adc5dea00000,10000000000000000,65,5v1j4f4ds7c000,194,199",
    "radix-tiny=1076,0.000001ad7f29abcaf48,0.0000000000",
    "radix-special=NaN,Infinity,-Infinity,0,0",
    "radix-id=true",
    "radix-ids=true",
    "radix-hot=37906",
    "ties=600479950316068.2,-600479950316035.2,1286742750677258.2,600479950316068.2,6.004799503160682e+14,0.3,1.5,123.456",
    "ties-hot=600479950316068.2",
    "bigpow=RangeError,RangeError,RangeError,1,-1,0,1267650600228229401496703205376",
    "bigpow-hot=RangeError",
    "join-pair=d83d.de00",
    "join-round=61.d83d.62.de00.63,63.de00.62.d83d.61,61.d83d.62.de00.63",
    "join-sep=78.d83d.79,d83d.2c.de00,d83d.de00",
    "join-nullish=2d.d83d.2d,d83d.d83d",
    "join-arraylike=d83d.de00",
    "join-tolocale=d83d.2c.de00",
    "join-typedarray=31.d83d.32,1,2",
    "join-hot=d83d.de00",
    "raw=61.d83d.62.de00,78.d83d.2d.de00.79",
    "re-tmpl=61.d83d.63,61.d83d.de00.63",
    "re-sub=3c.61.3e.d83d.62,61.d83d.5b.61.d83d.5d,5b.d83d.63.5d.d83d.63",
    "re-group=5b.61.7c.d83d.5d.62,61.5b.d83d.5d,61.5b.7c.d83d.5d",
    "re-protocol=3c.61.3e.d83d.62,61.5b.d83d.5d",
    "re-hot=61.d83d.5b.61.d83d.5d",
    "sym-split=own,proto,proto,sub,x|y",
    "split-hot=x|y",
    "hypot=1.4142135623730952e+300,5e+300,5e-200,1.414213562373095e-200,1e-300",
    "hypot-edge=0,0,0,Infinity,Infinity,NaN,5,5e+300",
    "hypot-hot=5e+300",
    "trim-ws=3,1,1,1",
    "num-ws=NaN,NaN,1,0,2",
    "bigint-ws=1,SyntaxError,SyntaxError,3,0",
    "bigint-cmp=true,false,false,false",
    "num-ws-hot=NaN",
];

#[test]
fn tier_probe_child() {
    if std::env::var_os("ZIPP_STRINGS_JSON_TIER_CHILD").is_none() {
        return;
    }
    assert_eq!(run_ok(TIER_PROBE), [TIER_EXPECTED.join(";")]);
}

#[test]
fn tier_probe_matches_node_in_every_mode() {
    if std::env::var_os("ZIPP_STRINGS_JSON_TIER_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("default", None),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "tier_probe_child", "--nocapture"])
            // An inherited setting must not decide what a mode tests.
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env("ZIPP_STRINGS_JSON_TIER_CHILD", "1");
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "tier_probe_child/{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Every string operation below against a reference written over UTF-16 code
/// units (`charCodeAt`, `+` and `slice` are exact), on random strings mixing
/// lone surrogates, both halves of a pair, a real U+FFFD and `$` selectors.
const SURROGATE_PROBE: &str = r##"
var failures = [];
var checks = 0;
function units(s) { var o = []; for (var i = 0; i < s.length; i++) o.push(s.charCodeAt(i)); return o; }
function hex(s) { return units(s).map(function (u) { return u.toString(16); }).join("."); }
function same(label, actual, expected) {
  checks++;
  if (actual !== expected) failures.push(label + ": " + actual + " != " + expected);
}
function matchAt(S, N, i) {
  if (i < 0 || i + N.length > S.length) return false;
  for (var j = 0; j < N.length; j++) if (S[i + j] !== N[j]) return false;
  return true;
}
function clamp(p, len) { p = Math.trunc(p); if (!(p > 0)) return 0; return p > len ? len : p; }
function refIndexOf(s, n, from) {
  var S = units(s), N = units(n);
  for (var i = clamp(from, S.length); i + N.length <= S.length; i++) if (matchAt(S, N, i)) return i;
  return -1;
}
function refLastIndexOf(s, n, pos) {
  var S = units(s), N = units(n);
  for (var i = Math.min(clamp(pos, S.length), S.length - N.length); i >= 0; i--) if (matchAt(S, N, i)) return i;
  return -1;
}
function refSplit(s, n) {
  var S = units(s), N = units(n), out = [], last = 0;
  if (N.length === 0) return S.map(function (u) { return u.toString(16); }).join("|");
  for (var i = 0; i + N.length <= S.length;) {
    if (matchAt(S, N, i)) { out.push(hex(s.slice(last, i))); i += N.length; last = i; } else i++;
  }
  out.push(hex(s.slice(last)));
  return out.join("|");
}
function subst(tmpl, matched, pre, post) {
  var out = "";
  for (var i = 0; i < tmpl.length; i++) {
    var c = tmpl[i], d = tmpl[i + 1];
    if (c === "$" && d === "$") { out += "$"; i++; }
    else if (c === "$" && d === "&") { out += matched; i++; }
    else if (c === "$" && d === "`") { out += pre; i++; }
    else if (c === "$" && d === "'") { out += post; i++; }
    else out += c;
  }
  return out;
}
function refReplace(s, n, r, all) {
  var S = units(s), N = units(n), out = "", last = 0, i = 0;
  var fn = typeof r === "function";
  function rep(pos) { return fn ? String(r(n, pos, s)) : subst(r, n, s.slice(0, pos), s.slice(pos + n.length)); }
  if (N.length === 0) {
    if (!all) return rep(0) + s;
    for (var p = 0; p <= S.length; p++) { out += rep(p); if (p < S.length) out += s[p]; }
    return out;
  }
  while (i + N.length <= S.length) {
    if (matchAt(S, N, i)) {
      out += s.slice(last, i) + rep(i);
      i += N.length; last = i;
      if (!all) break;
    } else i++;
  }
  return out + s.slice(last);
}
var alphabet = ["a", "b", "\uD800", "\uDC00", "\uD83D", "\uDE00", "\uD83D\uDE00", "\uFFFD", "\u00e9", " ", "$", "&", "`", "'", "\u0301", "I", "i", "\u0130"];
var seed = 7;
function rnd(n) { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return seed % n; }
function rs(n) { var s = ""; for (var i = 0; i < n; i++) s += alphabet[rnd(alphabet.length)]; return s; }
for (var k = 0; k < 400; k++) {
  var s = rs(rnd(10)), n = rs(rnd(3)), r = rs(rnd(4)), p = rnd(12) - 1;
  var tag = hex(s) + "/" + hex(n);
  same("indexOf " + tag, s.indexOf(n), refIndexOf(s, n, 0));
  same("indexOf@ " + tag + "@" + p, s.indexOf(n, p), refIndexOf(s, n, p));
  same("lastIndexOf " + tag, s.lastIndexOf(n), refLastIndexOf(s, n, Infinity));
  same("lastIndexOf@ " + tag + "@" + p, s.lastIndexOf(n, p), refLastIndexOf(s, n, p));
  same("includes@ " + tag + "@" + p, s.includes(n, p), refIndexOf(s, n, p) !== -1);
  same("startsWith@ " + tag + "@" + p, s.startsWith(n, p), matchAt(units(s), units(n), clamp(p, s.length)));
  var e = clamp(p, s.length);
  same("endsWith@ " + tag + "@" + p, s.endsWith(n, p), matchAt(units(s), units(n), e - n.length));
  same("split " + tag, s.split(n).map(hex).join("|"), refSplit(s, n));
  same("replace " + tag, hex(s.replace(n, r)), hex(refReplace(s, n, r, false)));
  same("replaceAll " + tag, hex(s.replaceAll(n, r)), hex(refReplace(s, n, r, true)));
  var f = function (m, pos) { return "<" + pos + hex(m) + ">"; };
  same("replace-fn " + tag, hex(s.replace(n, f)), hex(refReplace(s, n, f, false)));
  same("replaceAll-fn " + tag, hex(s.replaceAll(n, f)), hex(refReplace(s, n, f, true)));
  same("padStart " + tag, hex(s.padStart(s.length + 3, n || "x")), hex((n || "x").repeat(3).slice(0, 3) + s));
}
// Case mapping and normalization copy lone surrogates through and map the rest
// exactly as the well-formed runs map on their own.
function runs(s, f) {
  var out = "", run = "";
  for (var i = 0; i < s.length; i++) {
    var u = s.charCodeAt(i), v = s.charCodeAt(i + 1);
    if (u >= 0xD800 && u <= 0xDBFF && v >= 0xDC00 && v <= 0xDFFF) { run += s[i] + s[i + 1]; i++; }
    else if (u >= 0xD800 && u <= 0xDFFF) { out += f(run) + s[i]; run = ""; }
    else run += s[i];
  }
  return out + f(run);
}
for (var k = 0; k < 200; k++) {
  var s = rs(rnd(10)), tag = hex(s);
  same("toLocaleUpperCase " + tag, hex(s.toLocaleUpperCase()), hex(s.toUpperCase()));
  same("toLocaleLowerCase " + tag, hex(s.toLocaleLowerCase()), hex(s.toLowerCase()));
  same("toLocaleUpperCase-tr " + tag, hex(s.toLocaleUpperCase("tr")), hex(runs(s, function (x) { return x.toLocaleUpperCase("tr"); })));
  same("toLocaleLowerCase-lt " + tag, hex(s.toLocaleLowerCase("lt")), hex(runs(s, function (x) { return x.toLocaleLowerCase("lt"); })));
  same("NFD " + tag, hex(s.normalize("NFD")), hex(runs(s, function (x) { return x.normalize("NFD"); })));
  same("NFKC " + tag, hex(s.normalize("NFKC")), hex(runs(s, function (x) { return x.normalize("NFKC"); })));
}
// The audit's probes, verbatim (expected values are node's).
var R = "\uFFFD";
same("probe-includes", ("a" + R + "b").includes("\uD800"), false);
same("probe-replace-needle", hex(("a" + R + "b").replace("\uD800", "X")), "61.fffd.62");
same("probe-replace-value", hex("abc".replace("b", "\uD800")), "61.d800.63");
same("probe-replaceAll-empty", hex("\uD800\uDC00a".replaceAll("", "-")), "2d.d800.2d.dc00.2d.61.2d");
same("probe-localeCompare", "\uD800".localeCompare("\uDBFF"), -1);
same("probe-localeCompare-fffd", "\uD800".localeCompare(R) !== 0, true);
// Not equal to a private-use character either; after the pair it was cut from
// (node), and Intl.Collator.prototype.compare agrees with localeCompare.
same("localeCompare-pua", "\uD800".localeCompare("\uE000") !== 0, true);
same("localeCompare-half-pair", "\uD83D".localeCompare("\uD83D\uDE00"), 1);
var coll = new Intl.Collator();
for (var k = 0; k < 200; k++) {
  var s = rs(rnd(6)), t = rs(rnd(6));
  same("Collator.compare " + hex(s) + " " + hex(t), coll.compare(s, t), s.localeCompare(t));
}
same("probe-normalize", hex("\uD800\u0301".normalize("NFD")), "d800.301");
same("probe-toLocaleUpperCase", hex("a\uD800".toLocaleUpperCase("tr")), "41.d800");
console.log(checks + " checks, " + failures.length + " failures");
for (var i = 0; i < Math.min(failures.length, 10); i++) console.log(failures[i]);
"##;

#[test]
fn string_builtins_keep_lone_surrogates() {
    assert_eq!(run_ok(SURROGATE_PROBE), ["6610 checks, 0 failures"]);
}

const JSON_PROBE: &str = r##"
var lines = [];
function log(label, value) { lines.push(label + "=" + value); }
function hex(s) { var o = []; for (var i = 0; i < s.length; i++) o.push(s.charCodeAt(i).toString(16)); return o.join("."); }
function flat(s) { return s.split("\n").join("|"); }
// Callable values are undefined: omitted in objects, null in arrays.
log("callable-obj", JSON.stringify({ a: Object, b: Math.max, c: function () {}, d: Array, e: parseInt, f: Symbol, g: Date, h: 1 }));
log("callable-arr", JSON.stringify([Object, Math.max, Map, Promise, class {}, new Proxy(function () {}, {})]));
log("callable-top", String(JSON.stringify(Map)) + "," + String(JSON.stringify(class {})) + "," + String(JSON.stringify(new Proxy(function () {}, {}))));
log("callable-config", flat(JSON.stringify({ type: String, c: class {}, keep: [1] }, null, 1)));
log("proxy-object", JSON.stringify({ p: new Proxy({ x: 1 }, {}) }));
// toJSON is read only from an Object or a BigInt, never a string or symbol.
String.prototype.toJSON = function () { return "X"; };
log("tojson-string", JSON.stringify(["a", { k: "b" }]) + JSON.stringify("q") + JSON.stringify("q" + "rr".repeat(2)));
log("tojson-string-wrapper", JSON.stringify([new String("a")]));
delete String.prototype.toJSON;
Symbol.prototype.toJSON = function () { return "S"; };
log("tojson-symbol", JSON.stringify({ a: Symbol() }) + JSON.stringify([Symbol()]));
delete Symbol.prototype.toJSON;
BigInt.prototype.toJSON = function () { return "B" + this; };
log("tojson-bigint", JSON.stringify([1n]));
delete BigInt.prototype.toJSON;
// A String wrapper keeps its lone surrogates (\udXXX escapes).
log("wrapper-lone", JSON.stringify(new String("\uD800")) + JSON.stringify({ a: new String("x\uDC00y") }) + JSON.stringify([Object("\uD83D")]));
var w = new String("\uD800");
log("wrapper-tostring", [String(w).charCodeAt(0), ("" + w).charCodeAt(0), w.toString().charCodeAt(0)].join(","));
log("wrapper-custom", JSON.stringify({ a: Object.assign(new String("z"), { toString: function () { return "\uDBFF!"; } }) }));
// The gap is the first ten code UNITS of a string space.
log("gap-astral", JSON.stringify([1], null, "\u{1F600}".repeat(6)).split("\n")[1].length);
log("gap-lone", hex(JSON.stringify([1], null, "a\uD800bcdefghijk").split("\n")[1]));
log("gap-split", hex(JSON.stringify([1], null, "abcdefghi\u{1F600}").split("\n")[1]));
log("gap-nested", hex(JSON.stringify({ a: [1, { b: 2 }] }, null, "\uDC00-")));
log("gap-replacer", hex(JSON.stringify([1, [2]], function (k, v) { return v; }, "\uD83D")));
log("gap-allowlist", hex(JSON.stringify({ x: [1], y: 2 }, ["x"], "\uD800")));
log("gap-wrapper", flat(JSON.stringify([1], null, new String("--"))));
log("gap-control", hex(JSON.stringify([1], null, "")));
console.log(lines.join(";"));
"##;

/// node v24's output for [`JSON_PROBE`].
const JSON_EXPECTED: [&str; 20] = [
    r#"callable-obj={"h":1}"#,
    "callable-arr=[null,null,null,null,null,null]",
    "callable-top=undefined,undefined,undefined",
    r#"callable-config={| "keep": [|  1| ]|}"#,
    r#"proxy-object={"p":{"x":1}}"#,
    r#"tojson-string=["a",{"k":"b"}]"q""qrrrr""#,
    r#"tojson-string-wrapper=["X"]"#,
    "tojson-symbol={}[null]",
    r#"tojson-bigint=["B1"]"#,
    r#"wrapper-lone="\ud800"{"a":"x\udc00y"}["\ud83d"]"#,
    "wrapper-tostring=55296,55296,55296",
    r#"wrapper-custom={"a":"\udbff!"}"#,
    "gap-astral=11",
    "gap-lone=61.d800.62.63.64.65.66.67.68.69.31",
    "gap-split=61.62.63.64.65.66.67.68.69.d83d.31",
    "gap-nested=7b.a.dc00.2d.22.61.22.3a.20.5b.a.dc00.2d.dc00.2d.31.2c.a.dc00.2d.dc00.2d.7b.a.dc00.2d.dc00.2d.dc00.2d.22.62.22.3a.20.32.a.dc00.2d.dc00.2d.7d.a.dc00.2d.5d.a.7d",
    "gap-replacer=5b.a.d83d.31.2c.a.d83d.5b.a.d83d.d83d.32.a.d83d.5d.a.5d",
    "gap-allowlist=7b.a.1.d800.22.78.22.3a.20.5b.a.1.d800.1.d800.31.a.1.d800.5d.a.7d",
    "gap-wrapper=[|--1|]",
    "gap-control=5b.a.1.31.a.5d",
];

#[test]
fn json_stringify_matches_node() {
    assert_eq!(run_ok(JSON_PROBE), [JSON_EXPECTED.join(";")]);
}

/// Legacy `Date.parse` digit runs are bounded before any arithmetic. In a
/// debug build the wrapping arithmetic was an overflow panic.
#[test]
fn legacy_date_parse_rejects_absurd_fields() {
    let out = run_ok(
        r#"
        console.log([
            Date.parse("Jan 1 50505469855533201"),
            Date.parse("50505469855533201-01-01 00:00"),
            Date.parse("Jan 1 2000 10:00 +307445734561825861:00"),
            new Date("Jan 1 50505469855533201").getTime(),
            Date.parse("Jan 1 2000 10:00 +9223372036854775807"),
            Date.parse("Jan 1 2000 9223372036854775807:00"),
            Date.parse("Jan 9223372036854775807 2000"),
            Date.parse("9223372036854775807/1/1"),
        ].join());
        console.log([
            Date.parse("Jan 1 2000 10:00 GMT+0100"),
            Date.parse("Jan 1 2000 10:00 GMT+01:30"),
            Date.parse("Jan 1 2000 10:00 UTC"),
        ].join());
        "#,
    );
    assert_eq!(
        out,
        [
            "NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN",
            "946717200000,946715400000,946720800000"
        ]
    );
}

/// `Intl.Segmenter` iteration streams forward from the previous boundary.
/// The boundaries it reaches must be the ones the whole-string walk finds:
/// the repeated unit starts and ends on a break, so the long string's
/// segments are exactly its unit's, repeated.
#[test]
fn segment_iteration_is_the_whole_string_walk() {
    let out = run_ok(
        r#"
        function walk(s, g) {
          var n = 0, w = 0, rebuilt = "", last = -1, ordered = true;
          for (var x of new Intl.Segmenter("en", { granularity: g }).segment(s)) {
            n++;
            if (x.isWordLike) w++;
            if (x.index <= last || x.input !== s) ordered = false;
            last = x.index;
            rebuilt += x.segment;
          }
          return [n, w, rebuilt === s, ordered];
        }
        var unit = "can't stop 3.14 1,2 a.b \uD83D\uDE00e\u0301\r\n\uD800x! ";
        var big = unit.repeat(3000);
        var lines = [];
        ["grapheme", "word", "sentence"].forEach(function (g) {
          var one = walk(unit, g), all = walk(big, g);
          var n = g === "sentence" ? 1 : one[0] * 3000;
          var w = g === "word" ? one[1] * 3000 : 0;
          lines.push(g + ":" + (all[0] === n) + "," + (all[1] === w) + "," + all[2] + "," + all[3]);
        });
        var segs = new Intl.Segmenter("en", { granularity: "word" }).segment(big);
        var agree = true, k = 0;
        for (var x of segs) {
          var c = segs.containing(x.index);
          if (c.segment !== x.segment || c.index !== x.index || c.isWordLike !== x.isWordLike) agree = false;
          if (++k === 60) break;
        }
        lines.push("containing:" + agree);
        console.log(lines.join(";"));
        "#,
    );
    assert_eq!(
        out,
        ["grapheme:true,true,true,true;word:true,true,true,true;sentence:true,true,true,true;containing:true"]
    );
}

/// Recursion in `JSON.stringify` (including a replacer or `toJSON` that
/// returns a fresh object at every level), `JSON.parse` and its reviver walk
/// is bounded in the ordinary profile, so it ends in a catchable RangeError
/// instead of overflowing the native stack. The limits are sized for the
/// CLI's 256 MiB interpreter stack; the probe runs on a thread with room for
/// a debug build's larger frames.
#[cfg(not(feature = "safe-sandbox"))]
#[test]
fn deep_json_walks_throw_a_catchable_range_error() {
    const DEEP: &str = r#"
      var lines = [];
      function probe(label, f) {
        try { f(); lines.push(label + "=ok"); }
        catch (e) { lines.push(label + "=" + e.name); }
      }
      var N = 10001;
      probe("stringify-array", function () { var a = []; for (var i = 0; i < N; i++) a = [a]; JSON.stringify(a); });
      probe("stringify-object", function () { var o = {}; for (var i = 0; i < N; i++) o = { x: o }; JSON.stringify(o); });
      probe("stringify-indent", function () { var o = {}; for (var i = 0; i < N; i++) o = { x: o }; JSON.stringify(o, null, 1); });
      probe("stringify-replacer", function () {
        JSON.stringify({ a: 1 }, function (k, v) { return typeof v === "number" ? { n: v } : v; });
      });
      probe("stringify-tojson", function () {
        var mk = function () { return { toJSON: function () { return { x: mk() }; } }; };
        JSON.stringify(mk());
      });
      probe("stringify-under", function () { var a = []; for (var i = 0; i < 9000; i++) a = [a]; JSON.stringify(a); });
      var P = 100001;
      probe("parse-array", function () { JSON.parse("[".repeat(P) + "]".repeat(P)); });
      probe("parse-object", function () { JSON.parse('{"a":'.repeat(P) + "1" + "}".repeat(P)); });
      probe("parse-reviver", function () { JSON.parse("[".repeat(P) + "]".repeat(P), function (k, v) { return v; }); });
      probe("parse-under", function () { JSON.parse("[".repeat(5000) + "]".repeat(5000), function (k, v) { return v; }); });
      console.log(lines.join(";"));
    "#;
    let out = std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(|| run_ok(DEEP))
        .expect("spawn big-stack thread")
        .join()
        .expect("probe thread");
    assert_eq!(
        out,
        ["stringify-array=RangeError;stringify-object=RangeError;stringify-indent=RangeError;\
stringify-replacer=RangeError;stringify-tojson=RangeError;stringify-under=ok;\
parse-array=RangeError;parse-object=RangeError;parse-reviver=RangeError;parse-under=ok"]
    );
}

/// The hardened profile's caps meet every builder before it allocates: a
/// failed infallible allocation traps the WebAssembly host and poisons the
/// `Engine`. Sizes derive from the live ceilings.
#[cfg(feature = "safe-sandbox")]
mod hardened {
    use super::run_ok;
    use zipp_vm::embed::{self, ScriptState};
    use zipp_vm::safe_native_limits::{MAX_DENSE_ARRAY_LEN as DENSE, MAX_STRING_BYTES as BYTES};

    fn assert_catchable_range_error(operation: &str) {
        let source = format!(
            r#"
            try {{
                {operation};
                console.log("completed");
            }} catch (error) {{
                console.log(error instanceof RangeError ? "range" : "other: " + error);
            }}
            "#
        );
        assert_eq!(run_ok(&source), ["range"], "operation: {operation}");
    }

    #[test]
    fn split_part_count_is_admitted_before_building() {
        // DENSE + 1 parts from a string well inside the byte cap.
        assert_catchable_range_error(&format!("','.repeat({DENSE}).split(',')"));
        assert_catchable_range_error(&format!("'x,'.repeat({DENSE}).split(',')"));
        assert_catchable_range_error(&format!("'a'.repeat({}).split('')", DENSE + 1));
        // A lone-surrogate separator is matched per code unit (here also the
        // trail half of every pair), and admitted too.
        assert_catchable_range_error(&format!("'\\uD83D\\uDE00'.repeat({DENSE}).split('\\uDE00')"));
        assert_catchable_range_error(&format!("'a\\uD800'.repeat({DENSE}).split('\\uD800')"));
        // At the cap it still works.
        assert_eq!(
            run_ok(&format!("console.log(','.repeat({}).split(',').length)", DENSE - 1)),
            [DENSE.to_string()]
        );
    }

    #[test]
    fn concat_is_admitted_before_each_append() {
        // The doubling shape: repeated self-concat asked the allocator for
        // 137 GB and ABORTED the process, where node raises RangeError.
        assert_catchable_range_error(
            "(() => { let s = 'x'.repeat(1 << 16); for (let i = 0; i < 40; i++) s = s.concat(s); return s.length; })()",
        );
        // One argument past the cap, and many small ones that sum past it.
        assert_catchable_range_error(&format!(
            "'a'.repeat({}).concat('a'.repeat({}))",
            BYTES / 2,
            BYTES / 2 + 1
        ));
        assert_catchable_range_error(&format!(
            "'a'.repeat({}).concat(...Array(64).fill('a'.repeat({})))",
            BYTES / 2,
            BYTES / 64
        ));
        // A `toString` returning the oversized part is charged the same way.
        assert_catchable_range_error(&format!(
            "'a'.repeat({}).concat({{ toString() {{ return 'a'.repeat({}); }} }})",
            BYTES / 2,
            BYTES / 2 + 1
        ));
        // Just under the cap still concatenates.
        assert_eq!(
            run_ok(&format!(
                "console.log('a'.repeat({}).concat('b').length)",
                BYTES - 2
            )),
            [(BYTES - 1).to_string()]
        );
    }

    #[test]
    fn locale_case_mapping_obeys_the_byte_cap() {
        // U+0390 uppercases to three code points: six bytes from two.
        assert_catchable_range_error(&format!(
            "'\\u0390'.repeat({}).toLocaleUpperCase()",
            BYTES / 6 + 1
        ));
        assert_catchable_range_error(&format!(
            "'\\u0390'.repeat({}).toLocaleUpperCase('lt')",
            BYTES / 6 + 1
        ));
    }

    #[test]
    fn pad_obeys_the_byte_cap() {
        // A three-byte filler: the unit count fits, the bytes do not (the
        // receiver is one of the target's units).
        assert_catchable_range_error(&format!("'a'.padStart({}, '\\u20ac')", BYTES / 3 + 2));
        assert_catchable_range_error(&format!("'a'.padEnd({}, '\\u20ac')", BYTES / 3 + 2));
        // One byte under the cap still builds.
        assert_eq!(
            run_ok(&format!("console.log('a'.padStart({}, '\\u20ac').length)", BYTES / 3)),
            [(BYTES / 3).to_string()]
        );
    }

    const CEILING: usize = 64 << 20;

    fn slot(state: &ScriptState, name: &str) -> u32 {
        state
            .symbols()
            .into_iter()
            .find(|symbol| symbol.name == name)
            .unwrap_or_else(|| panic!("missing global function {name}"))
            .index
    }

    /// Run `body` once under the ceiling; it must be convicted by the heap
    /// ceiling before the native builder commits memory far past it.
    fn assert_convicted_under_the_ceiling(body: &str) {
        let source = format!("let held = []\nfunction run() {{ {body}; return held.length }}\n");
        let mut state = embed::compile_script(&source).expect("script compiles");
        state.disable_vm_jit();
        state.run_init().expect("script initializes");
        state.set_limits(u64::MAX, None);
        state.set_heap_limit(CEILING);
        let run = slot(&state, "run");
        let result = state.call_slot(run, &[]);
        let peak = state.heap_bytes();
        match result {
            Err(message) => assert!(
                message.contains("memory budget"),
                "{body}: expected the heap ceiling to convict, got: {message}"
            ),
            Ok(value) => panic!("{body}: completed with {value:?} at {peak} bytes"),
        }
        assert!(
            peak <= CEILING + CEILING / 2,
            "{body}: the builder ran away to {peak} bytes under a {CEILING}-byte ceiling"
        );
    }

    #[test]
    fn split_meets_the_heap_ceiling_before_building() {
        assert_convicted_under_the_ceiling(&format!(
            "held.push(','.repeat({}).split(','))",
            DENSE - 2
        ));
    }

    #[test]
    fn pad_meets_the_heap_ceiling_before_building() {
        assert_convicted_under_the_ceiling(&format!(
            "held.push('a'.padStart({}, '\\u20ac'))",
            CEILING / 2
        ));
    }
}
