//! `<`, `<=`, `>`, `>=` in compiled code: two strings answer there, whatever
//! their contents (ropes, surrogates, astral characters), and everything else
//! keeps its coercions, in order, through the interpreter. Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"true,true,false,false,lt,-1,3
false,false,true,true,gt,1,0
false,true,false,true,le,0,0
false,true,false,true,le,0,0
true,true,false,false,lt,-1,3
false,false,true,true,gt,1,0
true,true,false,false,lt,-1,3
false,false,true,true,gt,1,0
true,true,false,false,lt,-1,3
false,true,false,true,le,0,0
false,false,true,true,gt,1,0
true,true,false,false,lt,-1,3
false,false,true,true,gt,1,0
false,false,true,true,gt,1,0
true,true,false,false,lt,-1,3
true,true,false,false,lt,-1,3
false,false,true,true,gt,1,0
false,false,true,true,gt,1,0
true,true,false,false,lt,-1,3
true,true,false,false,lt,-1,3
true,true,false,false,lt,-1,3
false,false,true,true,gt,1,0
false,true,false,true,le,0,0
true,true,false,false,lt,-1,3
true,true,false,false,lt,-1,3
false,false,true,true,gt,1,0
true,true,false,false,lt,-1,3
false,false,false,false,none,0,0
false,false,false,false,none,0,0
false,true,false,true,le,0,0
false,true,false,true,le,0,0
false,true,false,true,le,0,0
true,true,false,false,lt,-1,3
false,false,true,true,gt,1,0
false,true,false,true,le,0,0
false,false,false,false,none,0,0
false,false,false,false,none,0,0
false,false,false,false,none,0,0
false,true,false,true,le,0,0
false,false,true,true,gt,1,0
false,true,false,true,le,0,0
false,true,false,true,le,0,0
false,false,false,false,none,0,0
false,false,false,false,none,0,0
false,false,false,false,none,0,0
false,false,true,true,gt,1,0
false,true,false,true,le,0,0
false,true,false,true,le,0,0
true,true,false,false,lt,-1,3
false,false,true,true,gt,1,0
true,true,false,false,lt,-1,3
false,false,true,true,gt,1,0
true,true,false,false,lt,-1,3
false,false,false,false,none,0,0
false,false,true,true,gt,1,0
false,false,true,true,gt,1,0
false,false,true,true,gt,1,0
false,false,true,true,gt,1,0
true,true,false,false,lt,-1,3
false,true,false,true,le,0,0
false,true,false,true,le,0,0
true,true,false,false,lt,-1,3
false,false,true,true,gt,1,0
true,true,false,false,lt,-1,3
true l,r
true l,r
false l,r
false l,r
false l,r
true l,r
false l,r
true l,r
RangeError
RangeError
RangeError
RangeError
TypeError
TypeError
w0 w999 689
w0 w999 689
w0 w999 689
w0 w999 689
1,2,3,a,b,c,d,
no throw
3004
40
88.201171875 2"####;

const PROGRAM: &str = r####"// Relational ops in hot compiled code: strings, ropes, and everything that is neither.
const HOT = 4000;
function hot(f) { var r; for (var i = 0; i < HOT; i++) r = f(i); return r; }
function lt(a, b) { return a < b; }
function le(a, b) { return a <= b; }
function gt(a, b) { return a > b; }
function ge(a, b) { return a >= b; }
function branch(a, b) { if (a < b) return "lt"; if (a <= b) return "le"; if (a > b) return "gt"; if (a >= b) return "ge"; return "none"; }
function tern(a, b) { return a < b ? -1 : a > b ? 1 : 0; }
function loop(a, b) { var n = 0; while (a < b && n < 3) n++; return n; }
var fns = [lt, le, gt, ge, branch, tern, loop];
hot((i) => { for (var f of fns) f("a" + (i % 7), "a" + (i % 5)); });
hot((i) => { for (var f of fns) f(i, i + 1); });
function show(a, b) { return fns.map((f) => f(a, b)).join(); }
var rope = "abc"; for (var i = 0; i < 40; i++) rope += "def";
var rope2 = "abc"; for (var i = 0; i < 40; i++) rope2 += "def";
var pairs = [
  ["a", "b"], ["b", "a"], ["a", "a"], ["", ""], ["", "a"], ["a", ""], ["abc", "abd"], ["abc", "ab"], ["ab", "abc"],
  [rope, rope2], [rope + "x", rope2], [rope2, rope + "x"], [rope, "abc"],
  ["é", "z"], ["é", "ê"], ["\u{1F600}", "￿"], ["￿", "\u{1F600}"], ["", "\u{10000}"], ["\u{10000}", ""],
  ["\uD800", "\uDC00"], ["\uD83D", "\u{1F600}"], ["\u{1F600}", "\uD83D"], ["😀", "\u{1F600}"], ["a\uD800b", "a\uD800c"],
  ["10", "9"], ["10", 9], [9, "10"], ["a", 1], [1, "a"], ["", 0], [0, ""], [" 5", 5],
  [1, 2], [2, 1], [1.5, 1.5], [NaN, 1], [1, NaN], [NaN, NaN], [-0, 0], [Infinity, -Infinity],
  [null, 0], [0, null], [undefined, 0], [0, undefined], [null, undefined], [true, false], [true, 1], ["1", true],
  [1n, 2], [2, 1n], ["1", 2n], [2n, "1"], [1n, 2n], ["x", 1n], [10000000000000000000n, 9999999999999999999n],
  [[2], [10]], [[2], "10"], ["2", [10]], [{}, "a"], ["[object Object]", {}], [{}, {}],
  [new String("a"), "b"], ["b", new String("a")], [new String("a"), new String("b")],
];
for (var [a, b] of pairs) console.log(show(a, b));
// Order of coercion, once each, and what they return.
var log = [];
var mk = (n, v) => ({ valueOf() { log.push(n); return v; } });
for (var f of [lt, le, gt, ge]) { log = []; console.log(f(mk("l", "a"), mk("r", "b")), log.join()); }
for (var f of [lt, le, gt, ge]) { log = []; console.log(f(mk("l", 1), mk("r", 1)), log.join()); }
var ts = { toString() { throw new RangeError("ts"); }, valueOf() { return this; } };
for (var f of [lt, le, gt, ge]) { try { f(ts, "a"); } catch (e) { console.log(e.name); } }
try { lt(Symbol("s"), "a"); } catch (e) { console.log(e.name); }
try { lt("a", Symbol("s")); } catch (e) { console.log(e.name); }
// Sorting strings in a comparator, a key, a rope key, a throwing comparator.
function strCmp(a, b) { return a < b ? -1 : a > b ? 1 : 0; }
var words = []; for (var i = 0; i < 2000; i++) words.push("w" + ((i * 7919) % 2003));
var rs = words.map((w, i) => ({ name: w, i }));
for (var r = 0; r < 4; r++) {
  var s = rs.slice().sort((a, b) => strCmp(a.name, b.name));
  console.log(s[0].name, s[1999].name, s[1000].i);
}
var mixed = ["b", 3, "a", 1, "c", 2, null, "d"];
console.log(mixed.slice().sort((a, b) => strCmp(String(a), String(b))).join());
try { ["a", {}, "b"].sort(strCmp); console.log("no throw"); } catch (e) { console.log(e.name); }
// One site sees strings, then numbers, then strings again, past the deopt limit.
function flip(a, b) { return a < b; }
var c = 0;
for (var i = 0; i < 3000; i++) { if (flip("x" + i, "x") ) c++; if (flip(i, 5)) c++; if (flip(null, i)) c++; if (flip(i + "", i)) c++; }
console.log(c);
// A string compare beside an append, and in a loop with doubles live.
function appendCmp(s, n) { var out = ""; var c = 0; for (var i = 0; i < n; i++) { out += s[i % s.length]; if (s > "a") c++; } return out.length + c; }
console.log(hot(() => appendCmp("hello", 20)));
function doubles(a, b, n) { var x = 0.5, y = 1.5; for (var i = 0; i < n; i++) { if (a < b) x = x * 1.5 + y; else y = y / 1.5; if (a > b) y += x; } return x + y; }
console.log(hot(() => doubles("p", "q", 8)), hot(() => doubles("q", "p", 8)));
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
