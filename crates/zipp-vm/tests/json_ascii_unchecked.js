// JSON ASCII-unchecked / integer-token oracle: `JSON.parse` and
// `JSON.stringify` must produce the same bytes whether a parser-proved ASCII
// view, a direct integer read, or the general `from_utf8` + `parse::<f64>`
// path served them. Every case here is one where the shortcut and the
// general path could plausibly disagree: non-ASCII and escaped names, lone
// surrogates, controls, `-0`, 15/16/17-digit integers, fractions, exponents,
// ropes, toJSON and revivers.
var out = [];
function rec(label, v) { out.push(label + ":" + v); }
function rt(s) { return JSON.stringify(JSON.parse(s)); }

// A. plain ASCII names and values round-trip byte-for-byte
var a = '{"alpha":1,"beta_2":"x-17","g":[true,false,null],"":""}';
rec("A", rt(a) + "|" + (rt(a) === a));

// B. non-ASCII names and values (2-, 3- and 4-byte UTF-8)
var b = '{"é":"café","中文":"😀","k":"aéb"}';
var bo = JSON.parse(b);
rec("B", rt(b) + "|" + Object.keys(bo).join(",") + "|" + bo["中文"].length);

// C. escapes in names and values: NUL, controls, quotes, backslash, slash
var c = '{"n\\u0000l":"\\u0000\\n\\t\\"\\\\\\/","\\u0001":"\\u001f\\b\\f\\r"}';
var co = JSON.parse(c);
rec("C", rt(c) + "|" + Object.keys(co).map(function (k) { return k.length; }).join(","));

// D. lone surrogates: escaped in the source, raw in a JS string. (A lone
// surrogate in a member NAME is a known pre-existing U+FFFD fold — B233's
// side-finding — the same with every latch, so it is not pinned here.)
var d = JSON.parse('{"s":"\\ud800x\\udc00","k":"\\udbff"}');
rec("D", JSON.stringify(d) + "|" + d.s.length + "|" + d.s.charCodeAt(0) + "|" +
  JSON.stringify("a\ud800b") + "|" + JSON.stringify({ q: "\udc00\ud83d" }));

// E. numbers: the integer fast lane's exact boundary and everything past it
var e = JSON.parse('[0,-0,7,-7,123456789012345,-999999999999999,1000000000000000,' +
  '9007199254740993,12345678901234567890123,1.5,-2.5,0.1,1e5,1E-2,1e21,1e-7,-1.25e+3,0.000001,1e400,-1e400]');
rec("E", JSON.stringify(e) + "|" + (1 / e[1]) + "|" + Object.is(e[1], -0) + "|" +
  e.map(function (n) { return typeof n; }).join(""));

// F. toJSON and a reviver see the same values on either path
var f = { a: 1, b: { toJSON: function () { return "tj-" + this.x; }, x: 5 }, d: new Date(0), s: "q" };
rec("F", JSON.stringify(f) + "|" + JSON.stringify(JSON.parse('{"a":1,"b":[2,3],"c":"z"}',
  function (k, v) { return typeof v === "number" ? v * 10 : k === "c" ? undefined : v; })));

// G. stringify strings: ASCII with escapes, a rope, DEL and U+2028/2029, long ASCII
var g1 = "he said \"hi\"\\ " + String.fromCharCode(7, 27) + " end";
var rope = ""; for (var i = 0; i < 40; i++) rope += "ab" + i;
var g3 = "x" + String.fromCharCode(0x7f) + "y" + String.fromCharCode(0x2028) + "z" + String.fromCharCode(0x2029) + "w"; // DEL and the line terminators are NOT escaped
var longA = ""; for (var j = 0; j < 300; j++) longA += String.fromCharCode(32 + (j % 95));
var gs = JSON.stringify([g1, rope, g3, longA]);
function codes(s) { var r = []; for (var ci = 0; ci < s.length; ci++) r.push(s.charCodeAt(ci)); return r.join("."); }
rec("G", gs.length + "|" + codes(JSON.stringify(g1)) + "|" + codes(JSON.stringify(g3)) + "|" +
  (JSON.parse(gs)[3] === longA) + "|" + JSON.parse(gs)[1].length);

// H. name ordering: canonical indices first, then textual; empty name and value
var h = JSON.parse('{"b":1,"10":2,"2":3,"":4,"-1":5,"01":6,"a":""}');
rec("H", Object.keys(h).join(",") + "|" + JSON.stringify(h));

// I. a bench-shaped tree, stringify -> parse -> stringify fixed point
function mulberry32(seed) { var s = seed | 0; return function () {
  s = (s + 0x6D2B79F5) | 0; var t = Math.imul(s ^ (s >>> 15), 1 | s);
  t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t; return ((t ^ (t >>> 14)) >>> 0) / 4294967296; }; }
var rnd = mulberry32(7); function ri(n) { return (rnd() * n) | 0; }
function build(depth) {
  if (depth >= 4) { var t = ri(6);
    return t === 0 ? ri(1000000) : t === 1 ? ri(2000) + 0.5 : t === 2 ? "w" + ri(1000) + "é" : t === 3 ? true : t === 4 ? -ri(50) : null; }
  if (ri(2) === 0) { var arr = []; for (var k = 0; k < 3; k++) arr.push(build(depth + 1)); return arr; }
  var o = {}; for (var m = 0; m < 3; m++) o["k" + ri(20) + "_" + m] = build(depth + 1); return o;
}
var tree = []; for (var r = 0; r < 12; r++) tree.push(build(0));
var s1 = JSON.stringify(tree), s2 = JSON.stringify(JSON.parse(s1));
var hh = 0x811c9dc5; for (var q = 0; q < s1.length; q++) hh = Math.imul(hh ^ s1.charCodeAt(q), 16777619);
rec("I", s1.length + "|" + (s1 === s2) + "|" + (hh >>> 0));

// J. number-grammar errors and boundaries still throw / parse as before
function tryParse(s) { try { return JSON.stringify(JSON.parse(s)); } catch (e) { return e.name; } }
rec("J", ["-", "-0", "01", "1.", ".5", "1e", "1e+", "0e0", "-0.0", "1E+2", "[1,]", '"\\ud800"', '"a"']
  .map(tryParse).join(","));

console.log(out.join("\n"));
