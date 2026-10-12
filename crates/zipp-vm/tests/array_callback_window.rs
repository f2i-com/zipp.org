//! An array builtin's compiled callback, called over one reused window.
//!
//! `map`, `filter`, `forEach`, `reduce`, the find family and `sort` call a
//! compiled callback natively over one register window that they keep for
//! every element, without a frame per call. Each call is still a fresh
//! activation, and three things a frame push and the native entry establish
//! per call were established once per builtin, or not at all:
//!
//! * A local the body reads before writing it must read `undefined`. The
//!   window kept the previous element's value: `function (x) { var t;
//!   if (x > 5) t = x; return t; }` mapped over `[7, 1]` gave `[7, 7]`.
//! * The body's direct global reads are valid only while the global is a
//!   plain slot. After `delete globalThis.g`, or a getter defined on it, the
//!   callback kept reading the slot (`NaN` where Node throws, the old value
//!   where Node calls the getter).
//! * A self-calling body assumes the name still names it.
//!
//! A capturing closure used to be refused outright and paid a frame and the
//! interpreter loop's entry per element, three to four times what Node pays.
//! It now runs over the window too, with its cells installed as the native
//! cross-call installs them.
//!
//! The expected lines are Node 24's output for the same program.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    #[cfg(feature = "safe-sandbox")]
    state.set_limits(4_000_000_000, None);
    state.run_init().expect("source runs");
    state.take_output()
}

const PROGRAM: &str = r####"// Differential probe: array builtins calling compiled callbacks, capturing and not.
var out = [];
function log(label, v) { out.push(label + ": " + v); }
function mk(n) { var a = []; for (var i = 0; i < n; i++) a.push((i * 7) % 11); return a; }
var big = mk(300);
function heat(f) { for (var i = 0; i < 400; i++) f(); }

// 1. A local read before it is written on some path (non-capturing and capturing).
function stale(x) { var t; if (x > 5) t = x; return t === undefined ? -1 : t; }
heat(function () { big.map(stale); });
log("stale map", big.map(stale).slice(0, 12).join(","));
function mkStale(k) { return function (x) { var t; if (x > k) t = x + k; return t === undefined ? -1 : t; }; }
var st5 = mkStale(5);
heat(function () { big.map(st5); });
log("stale closure map", big.map(st5).slice(0, 12).join(","));
log("stale closure filter", big.filter(function (x) { var u; if (x & 1) u = true; return u; }).length);

// 2. Captured variable written by the callback, and read after.
function sums(arr) { var total = 0, n = 0; arr.forEach(function (x) { total += x; n++; }); return total + "/" + n; }
heat(function () { sums(big); });
log("forEach capture write", sums(big));

// 3. Captured variable changed between builtin calls and during one.
function scale(arr, k) { return arr.map(function (x) { return x * k; }); }
heat(function () { scale(big, 3); });
log("map capture", scale(big, 3).slice(0, 6).join(",") + " | " + scale(big, 0.5).slice(0, 6).join(","));
function drift(arr) { var k = 1; return arr.map(function (x) { k += 1; return x + k; }); }
heat(function () { drift(big); });
log("map capture mutated during", drift(big).slice(0, 8).join(","));

// 4. reduce / some / every / find / findIndex / sort with capturing callbacks.
function red(arr, bias) { return arr.reduce(function (a, x) { return a + x + bias; }, 0); }
heat(function () { red(big, 2); });
log("reduce capture", red(big, 2) + " " + red(big, -1));
function srt(arr, dir) { var c = arr.slice(); c.sort(function (a, b) { return dir * (a - b); }); return c; }
heat(function () { srt(big, 1); });
log("sort capture", srt(big, 1).slice(0, 8).join(",") + " | " + srt(big, -1).slice(0, 8).join(","));
function fnd(arr, t) { return arr.find(function (x) { return x === t; }) + " " + arr.findIndex(function (x) { return x > t; }) + " " + arr.some(function (x) { return x > t + 100; }) + " " + arr.every(function (x) { return x >= 0 && x < t + 100; }); }
heat(function () { fnd(big, 4); });
log("find family capture", fnd(big, 4));

// 5. A throw from inside a capturing callback, caught outside; then reuse.
function thrower(arr, limit) {
  try { return arr.map(function (x, i) { if (i === limit) throw new RangeError("at " + i); return x; }).length; }
  catch (e) { return e.name + ":" + e.message; }
}
heat(function () { thrower(big, -1); });
log("throw in closure", thrower(big, 7) + " " + thrower(big, -1) + " " + thrower(big, 299));

// 6. A callback that calls the same builtin on the same closure (recursion).
function depthSum(node, k) { var s = node.v + k; node.kids.forEach(function (c) { s += depthSum(c, k); }); return s; }
var tree = { v: 1, kids: [{ v: 2, kids: [{ v: 3, kids: [] }, { v: 4, kids: [] }] }, { v: 5, kids: [] }] };
heat(function () { depthSum(tree, 1); });
log("recursive closure", depthSum(tree, 1) + " " + depthSum(tree, 10));

// 7. Globals: an implicit global deleted, then redefined with a getter, between calls and during one.
gg = 5;
function readG(x) { return x + gg; }
heat(function () { big.map(readG); });
log("global before", big.map(readG).slice(0, 4).join(","));
delete globalThis.gg;
try { log("global deleted", big.map(readG).length); } catch (e) { log("global deleted", "threw " + e.name); }
Object.defineProperty(globalThis, "gg", { get: function () { return 100; }, configurable: true });
log("global getter", big.map(readG).slice(0, 4).join(","));
delete globalThis.gg;
hh = 1;
function readH(x, i) { if (i === 3) delete globalThis.hh; return x + hh; }
function tryH() { hh = 1; try { return big.map(readH).length; } catch (e) { return "threw " + e.name; } }
for (var i = 0; i < 400; i++) { hh = 1; big.map(function (x) { return x + hh; }); }
log("global deleted during", tryH() + " " + tryH());

// 8. `this`: sloppy and strict, with and without thisArg; arrow lexical this.
function sloppyThis() { return this === globalThis ? "g" : typeof this; }
function strictThis() { "use strict"; return this === undefined ? "u" : typeof this; }
heat(function () { [1, 2].map(sloppyThis); [1, 2].map(strictThis); });
log("this", [1].map(sloppyThis)[0] + [1].map(strictThis)[0] + [1].map(sloppyThis, 5)[0] + [1].map(strictThis, 5)[0] + [1].map(strictThis, "s")[0]);
function Holder(k) { this.k = k; this.run = function (arr) { return arr.map((x) => x + this.k); }; }
var hold = new Holder(7);
heat(function () { hold.run(big); });
log("arrow this", hold.run(big).slice(0, 5).join(",") + " " + new Holder(1).run(big).slice(0, 5).join(","));

// 9. Missing and extra arguments, default parameters, arguments length.
function three(a, b, c) { return (a === undefined ? "u" : a) + "" + (b === undefined ? "u" : b) + (c === undefined ? "u" : typeof c); }
heat(function () { [1, 2].map(three); });
log("params", [4, 5].map(three).join(" ") + " " + [4].reduce(function (acc, x, i, arr, extra) { return acc + x + i + arr.length + (extra === undefined ? 0 : 99); }, 10));
function withDefault(k) { return function (x, i, arr, z = k) { return x + z; }; }
var wd = withDefault(3);
heat(function () { big.map(wd); });
log("default param closure", big.map(wd).slice(0, 5).join(","));

// 10. Closures over loop-scoped let bindings, one closure per iteration.
function perIter(arr) { var fs = []; for (let i = 0; i < 4; i++) fs.push(function (x) { return x + i; }); return fs.map(function (f) { return arr.map(f).slice(0, 3).join("."); }).join(" "); }
heat(function () { perIter(big); });
log("let closures", perIter(big));

// 11. A callback replaced or rebound while it runs: a self-recursive named function.
var fact = function f(n) { return n < 2 ? 1 : n * f(n - 1); };
heat(function () { [5, 6].map(function (x) { return fact(x); }); });
log("named fn expr", [5, 6].map(function (x) { return fact(x); }).join(","));
function selfc(n) { return n < 2 ? 1 : n + selfc(n - 1); }
heat(function () { [5, 6, 7].map(selfc); });
var keep = selfc;
log("self call", [5, 6, 7].map(keep).join(","));
selfc = function (n) { return -1; };
log("self call rebound", [5, 6, 7].map(keep).join(","));
selfc = keep;

// 12. Holes, getters on elements, and an array changed by the callback.
var holey = [1, , 3, , 5];
log("holes", holey.map(function (x) { return x * 2; }).join(",") + " " + holey.filter(function () { return true; }).length);
function shrink(arr) { var c = arr.slice(0, 10); return c.map(function (x, i) { if (i === 2) c.length = 5; return x; }).join(","); }
heat(function () { shrink(big); });
log("shrink during", shrink(big));

console.log(out.join("\n"));
"####;

#[test]
fn callbacks_over_a_reused_window_match_node() {
    let expected: &[&str] = &[
        r####"stale map: -1,7,-1,10,6,-1,9,-1,-1,8,-1,-1"####,
        r####"stale closure map: -1,12,-1,15,11,-1,14,-1,-1,13,-1,-1"####,
        r####"stale closure filter: 137"####,
        r####"forEach capture write: 1495/300"####,
        r####"map capture: 0,21,9,30,18,6 | 0,3.5,1.5,5,3,1"####,
        r####"map capture mutated during: 2,10,7,15,12,9,17,14"####,
        r####"reduce capture: 2095 1195"####,
        r####"sort capture: 0,0,0,0,0,0,0,0 | 10,10,10,10,10,10,10,10"####,
        r####"find family capture: 4 1 false true"####,
        r####"throw in closure: RangeError:at 7 300 RangeError:at 299"####,
        r####"recursive closure: 20 65"####,
        r####"global before: 5,12,8,15"####,
        r####"global deleted: threw ReferenceError"####,
        r####"global getter: 100,107,103,110"####,
        r####"global deleted during: threw ReferenceError threw ReferenceError"####,
        r####"this: guobjectnumberstring"####,
        r####"arrow this: 7,14,10,17,13 1,8,4,11,7"####,
        r####"params: 40object 51object 15"####,
        r####"default param closure: 3,10,6,13,9"####,
        r####"let closures: 0.7.3 1.8.4 2.9.5 3.10.6"####,
        r####"named fn expr: 120,720"####,
        r####"self call: 15,21,28"####,
        r####"self call rebound: 4,5,6"####,
        r####"holes: 2,,6,,10 3"####,
        r####"shrink during: 0,7,3,10,6,,,,,"####,
    ];
    let out = run(PROGRAM);
    let got: Vec<&str> = out.iter().flat_map(|chunk| chunk.lines()).collect();
    assert_eq!(got.len(), expected.len(), "{got:#?}");
    for (got, want) in got.iter().zip(expected) {
        assert_eq!(got, want);
    }
}
