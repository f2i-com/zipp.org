//! Regression coverage for the 15 September 2026 compiler audit (track
//! E-compile): evaluation order around register locals, capture analysis,
//! lexical TDZ, labelled statements, static class elements and parameter
//! scope.
//!
//! What is pinned, per probe group:
//! - ASSIGN_TARGET — an incremental array literal (`items = [...items, x]`)
//!   and `x = x++` no longer write the destination before reading it (R025,
//!   R283), including through `var` redeclarations and conditional arms.
//! - EVAL_ORDER — an operand or reference base/key held in a local's own
//!   register is fixed before a later operand reassigns it: `i + i++`,
//!   `a[i] = i++`, `o.x = (o = n, v)`, `x += (x = 10, 5)`, the switch
//!   discriminant (R027, R028).
//! - CAPTURE — closures in spread operands, `import()`, `#x in`, update
//!   targets, parameter defaults and `for` head defaults capture the enclosing
//!   binding, and a block-scoped shadow hides it only inside its block; each
//!   case has a same-named global a missed capture would silently use (R121,
//!   R035, R034, R016, R026, R017, R032, R039).
//! - TDZ — uncaptured `let`/`const`/`class` (and destructured leaves) are in
//!   their TDZ from scope entry, and a switch clause entered past a
//!   declaration throws (R019, R030, R280, R022).
//! - LABELS — `continue` to any label of a loop's label set (R007); a
//!   labelled block hoists its functions and disposes its `using` resources
//!   (R021, R052, R162), as does an arrow body; a `typeof` alias of the `then`
//!   arm does not reach the `else` arm (R029).
//! - CLASSES — static fields are defined, not assigned (R018); a static block
//!   over an enclosing local closes over it instead of panicking (R276).
//! - PARAM_SCOPE — parameter expressions give the body its own var
//!   environment (R023).
//! - DECORATORS — closures inside decorator expressions capture (R161).
//! - AWAIT_USING — `await using` disposal awaits what DisposeResources
//!   awaits: one tick for any number of null resources, none for a sync
//!   `using` result (R167), and an async arrow's body-level `await using` is
//!   awaited at all.
//! - MODULE — the strict-mode shapes of the above.
//!
//! Every group runs in a fresh child process under the default,
//! interpreter-only and forced-JIT modes. The compile-limit findings (R020,
//! R024, R041) are single-process tests below.

const CHILD_ENV: &str = "ZIPP_AUDIT_20260915_COMPILE_CHILD";

fn check_script(label: &str, src: &str, expected: &str) {
    let out = zipp_vm::run(src).unwrap_or_else(|e| panic!("{label}: compile failed: {e}"));
    assert!(
        out.error.is_none(),
        "{label}: runtime error: {:?}",
        out.error
    );
    assert_eq!(out.output.join("\n"), expected.trim(), "{label}");
}

fn check_module(label: &str, src: &str, expected: &str) {
    let out = zipp_vm::run_module_with_base(src, None)
        .unwrap_or_else(|e| panic!("{label}: compile failed: {e}"));
    assert!(
        out.error.is_none(),
        "{label}: runtime error: {:?}",
        out.error
    );
    assert_eq!(out.output.join("\n"), expected.trim(), "{label}");
}

const ASSIGN_TARGET: &str = r##"
function t(name, f) { try { console.log(name + " " + JSON.stringify(f())); } catch (e) { console.log(name + " THROW " + e.name); } }
// R025/R283: an incremental array literal assigned to a local it reads.
t("spread-append", function () { let items = ["a"]; items = [...items, "b"]; return items; });
t("spread-prepend", function () { let a = [1, 2]; a = [0, ...a]; return a; });
t("param-spread", function () { return (function (a, x) { a = [...a, x]; return a; })([1, 2], 3); });
t("cond-copy", function () { let x = [1]; x = x ? [...x] : []; return x; });
t("seq-arm", function () { let x = [1]; x = (0, [...x, 2]); return x; });
t("spread-member", function () { let x = { a: [1] }; x = [...x.a, 2]; return x; });
t("self-nest", function () { let a = [1]; a = [...[9], a]; return [a.length, a[1] === a]; });
t("throw-mid", function () { let a = [1, 2]; try { a = [...a, (() => { throw 1; })()]; } catch (e) {} return a; });
t("throw-iter", function () { let a = [1]; const bad = { [Symbol.iterator]() { throw new Error("x"); } }; try { a = [...bad]; } catch (e) {} return a; });
t("keys", function () { let n = 3; n = [...Array(n).keys()]; return n; });
t("arrow", function () { const f = (s) => { s = [...s].reverse(); return s; }; return f([1, 2, 3]); });
t("method", function () { class C { m(a) { a = [...a, "m"]; return a; } } return new C().m(["x"]); });
t("generator", function () { function* g(a) { a = [...a, "g"]; yield a; } return g(["y"]).next().value; });
t("for-update", function () { let a = [1]; for (let i = 0; i < 2; a = [...a, i], i++); return a; });
t("hot-accumulate", function () { let acc = []; for (let i = 0; i < 500; i++) acc = [...acc, i]; return [acc.length, acc[0], acc[499]]; });
// The same idiom through a var redeclaration and the other in-place forms.
t("var-arr-redecl", function () { var a = [1]; var a = [...a, 2]; return a; });
t("var-tmpl-redecl", function () { var s = "a"; var s = `${s}`; return s; });
t("var-logical-redecl", function () { var s = "k"; var m = new Map(); var s = m.get(s) || s; return s; });
t("var-loop-obj", function () { for (var i = 0; i < 3; i++) { var acc = { prev: acc, i: i }; } return acc; });
t("param-default-var", function () { return (function (a, b = [...a, 3]) { var a = [...a, 9]; return [a, b]; })([1]); });
t("destr-default", function () { let a = [1]; [a = [...a, 2]] = []; return a; });
t("obj-spread-self", function () { let o = { a: 1 }; o = { ...o, b: o.a }; return o; });
// `x = x++` / `x = --x`: the update must not write the destination it reads.
t("postinc-self", function () { let x = 5; x = x++; return x; });
t("postinc-dst", function () { let x = 5; let y = (x = x++); return [x, y]; });
t("postinc-loop", function () { let x = 0; for (let i = 0; i < 3; i++) x = x++; return x; });
t("predec-self", function () { let x = 5; x = --x; return x; });
t("postinc-str", function () { let x = "5"; x = x++; return [x, typeof x]; });
"##;
const ASSIGN_TARGET_EXPECTED: &str = r##"
spread-append ["a","b"]
spread-prepend [0,1,2]
param-spread [1,2,3]
cond-copy [1]
seq-arm [1,2]
spread-member [1,2]
self-nest [2,false]
throw-mid [1,2]
throw-iter [1]
keys [0,1,2]
arrow [3,2,1]
method ["x","m"]
generator ["y","g"]
for-update [1,0,1]
hot-accumulate [500,0,499]
var-arr-redecl [1,2]
var-tmpl-redecl "a"
var-logical-redecl "k"
var-loop-obj {"prev":{"prev":{"i":0},"i":1},"i":2}
param-default-var [[1,9],[1,3]]
destr-default [1,2]
obj-spread-self {"a":1,"b":1}
postinc-self 5
postinc-dst [5,5]
postinc-loop 0
predec-self 4
postinc-str [5,"number"]
"##;
const EVAL_ORDER: &str = r##"
function t(name, f) { try { console.log(name + " " + JSON.stringify(f())); } catch (e) { console.log(name + " THROW " + e.name); } }
// R027: an operand local is fixed before a later operand reassigns it.
t("add-postinc", function () { var i = 1; return i + i++; });
t("sub-postdec", function () { var v = 10; return v - v--; });
t("add-seq", function () { var x = 1; return x + (x = 10, 5); });
t("mul-compound", function () { let x = 2; return x * (x *= 3); });
t("gt", function () { let x = 5; return x > (x = 10); });
t("strict-ne", function () { var x = 5; return x !== (x = 10) ? "ne" : "eq"; });
t("bigint", function () { var a = 1n; return String(a + (a = 5n)); });
t("lt-jump", function () { var x = 1; if (x < (x = 0, 1)) return "T"; return "F"; });
t("le-jump", function () { var x = 1; for (var c = 0; x <= (x = 0, 1) && c < 3; c++); return c; });
t("while-gt", function () { var n = 3, c = 0; while (n > (n--, 0)) c++; return c; });
t("in", function () { var x = "a"; return x in (x = "b", { a: 1 }); });
t("instanceof", function () { var x = []; return x instanceof (x = 1, Array); });
t("concat-chain", function () { var s = "x"; return s + (s = "y") + "z"; });
t("computed-read", function () { var x = { k: "old" }; return x[(x = { k: "new" }, "k")]; });
t("computed-update", function () { var o = { k: 1 }, p = o; o[(o = { k: 100 }, "k")]++; return [p.k, o.k]; });
t("computed-delete", function () { var x = { k: 1 }, p = x; delete x[(x = { k: 2 }, "k")]; return [p.k, x.k]; });
t("computed-base-read", function () { let a = [1, 2], b = [10, 20]; return a[(a = b, 0)]; });
t("switch-disc", function () { var x = 1; switch (x) { case (x = 2, 2): return "case2"; case 1: return "case1"; } });
t("block-let", function () { { let q = 1; return q + q++; } });
t("param", function () { return (function (p) { return p + (p = 7); })(2); });
// R028: a member/compound assignment fixes its base, key and target first.
t("index-postinc", function () { var a = [10, 20, 30], i = 0; a[i] = i++; return a; });
t("index-chain", function () { let arr = [0, 0, 0], j = 0; arr[j] = j = 2; return arr; });
t("key-seq", function () { var o = {}, k = "a"; o[k] = (k = "b", 1); return o; });
t("base-seq", function () { var o = {}, p = o; o.x = (o = { n: "new" }, 1); return [p, o]; });
t("base-compound", function () { var o = { x: 1 }, p = o; o.x += (o = { x: 100 }, 5); return [p, o]; });
t("base-logical", function () { var q = { x: 0 }, p = q; q.x ||= (q = { x: 0 }, 9); return [p, q]; });
t("index-compound", function () { var a = [1, 1], i = 0; a[i] += (i = 1, 5); return [a, i]; });
t("index-compound-base", function () { let a = [1], b = [100]; a[0] += (a = b, 5); return [a, b]; });
t("private", function () { class C { #p = 0; static s(o) { o.#p = (o = new C(), 7); return o; } static g(o) { return o.#p; } } var a = new C(); var b = C.s(a); return [C.g(a), C.g(b)]; });
t("compound-seq", function () { var x = 1; x += (x = 10, 5); return x; });
t("compound-str", function () { var s = "a"; s += (s = "zz", "b"); return s; });
t("compound-exp", function () { var x = 1; x **= (x = 10, 2); return x; });
t("compound-postinc", function () { var x = 1; x += x++; return x; });
t("compound-ops", function () { var r = []; for (const op of ["-", "*", "/", "%", "<<", ">>", ">>>", "&", "|", "^"]) { r.push(new Function("var x = 12; x " + op + "= (x = 3, 2); return x;")()); } return r; });
// Unaffected neighbours keep their values.
t("call-args", function () { let x = 1; function f(a, b) { return [a, b]; } return f(x, x = 2); });
t("array-fixed", function () { let x = 1; return [x, x = 2]; });
t("template", function () { let x = 1; return `${x}${x = 2}`; });
t("typeof-same", function () { let x = 1; return typeof x === typeof (x = "s"); });
// Warm: the same shapes once the tiers have compiled them.
t("hot", function () {
  function add(i) { return i + i++; }
  function idx(a, i) { a[i] = i++; return a; }
  function cmp(x) { if (x < (x = 0, 1)) return 1; return 0; }
  function cpd(x) { x += (x = 10, 5); return x; }
  var s = 0, u = 0, c = 0, d = 0;
  for (var k = 0; k < 3000; k++) { s += add(k); u += idx([0, 0], 0)[0]; c += cmp(1); d += cpd(k); }
  return [s, u, c, d];
});
"##;
const EVAL_ORDER_EXPECTED: &str = r##"
add-postinc 2
sub-postdec 0
add-seq 6
mul-compound 12
gt false
strict-ne "ne"
bigint "6"
lt-jump "F"
le-jump 3
while-gt 3
in true
instanceof true
concat-chain "xyz"
computed-read "old"
computed-update [2,100]
computed-delete [null,2]
computed-base-read 1
switch-disc "case1"
block-let 2
param 9
index-postinc [0,20,30]
index-chain [2,0,0]
key-seq {"a":1}
base-seq [{"x":1},{"n":"new"}]
base-compound [{"x":6},{"x":100}]
base-logical [{"x":9},{"x":0}]
index-compound [[6,1],1]
index-compound-base [[100],[100]]
private [7,0]
compound-seq 6
compound-str "ab"
compound-exp 1
compound-postinc 2
compound-ops [10,24,6,0,48,3,3,0,14,14]
call-args [1,2]
array-fixed [1,2]
template "12"
typeof-same false
hot [8997000,0,0,4513500]
"##;
const CAPTURE: &str = r##"
function t(name, f) { try { console.log(name + " " + JSON.stringify(f())); } catch (e) { console.log(name + " THROW " + e.name); } }
// Same-named globals: a missed capture would silently read or write these.
var factor = 100, x = "GLOBAL", status = "global-status", total = 100, gb = "global-b", out = "global-out";
// R121/R035: closures inside spread operands.
t("spread-call", function () { function f(list, key) { return Math.max(...list.map(o => o[key])); } return f([{ a: 1 }, { a: 5 }], "a"); });
t("spread-array", function () { const factor = 3; return [...[1, 2].map(v => v * factor)]; });
t("spread-new", function () { const k = "z"; return new Array(...[1].map(() => k)); });
t("spread-push", function () { const k = 7; const a = []; a.push(...[1, 2].map(() => k)); return a; });
t("spread-super", function () { const k = 3; class A { constructor(...a) { this.a = a; } } class B extends A { constructor() { super(...[1].map(() => k)); } } return new B().a; });
t("spread-late-write", function () { let v = 1; const fs = [...[() => v]]; v = 9; return fs[0](); });
t("spread-redux", function () { function remove(state, id) { return { ...state, items: [...state.items.filter(i => i.id !== id)] }; } return remove({ items: [{ id: 1 }, { id: 2 }] }, 1).items; });
// R034: ImportCall, PrivateIn and update targets.
t("update-computed", function () { const k = 1; const a = [0, 0]; a[(() => k)()]++; return a; });
t("update-target-closure", function () { let v = 5; const o = { n: 1 }; (o.k = () => v, o).n++; return [o.n, o.k()]; });
t("private-in", function () { const o = {}; class C { #p; static t() { return #p in o; } } return C.t(); });
t("private-in-closure", function () { const o = {}; class C { #p; static t() { return #p in (() => o)(); } } return C.t(); });
t("import-call", function () { const spec = "./zipp-audit-missing-module.mjs"; const f = () => import(spec); const p = f(); p.catch(() => {}); return typeof p.then; });
// R016/R026: closures in parameter defaults see the parameters.
t("param-read", function () { function f(x, g = () => x) { return g(); } return f(5); });
t("param-write", function () { function w(x, set = v => { x = v; }) { set(9); return x; } return [w(1), x]; });
t("param-shadow-global", function () { function p5(gb, a = () => gb) { return a(); } return p5(5); });
t("param-arrow", function () { return ((n, f = () => n * 2) => f())(21); });
t("param-method", function () { class C { get(k, fb = () => k.toUpperCase()) { return fb(); } } return new C().get("abc"); });
t("param-self", function () { return (function (cb = () => typeof cb) { return cb(); })(); });
t("param-destructured", function () { return (function ({ a }, cb = () => a) { return cb(); })({ a: 4 }); });
t("param-rest", function () { return (function (cb = () => rest.length, ...rest) { return cb(); })(undefined, 1, 2); });
t("param-iife", function () { return (function (a, y = (() => a)()) { return y; })(9); });
t("param-hot", function () { function f(value, cmp = (o) => o === value) { return cmp(value); } let hits = 0; for (let i = 0; i < 3000; i++) if (f(i)) hits++; return hits; });
// R017/R032: a block-scoped shadow hides the outer binding only inside its block.
t("block-let-shadow", function () { let total = 10; const add = xs => { if (xs.length) { let total = 0; } total += 1; return total; }; add([1]); add([]); return [total, globalThis.total]; });
t("catch-shadow", function () { var status = "idle"; const run = () => { try { throw 0; } catch (status) {} status = "done"; }; run(); return [status, globalThis.status]; });
t("const-shadow", function () { const out = []; [1, 2, 3].forEach(item => { if (item > 1) { const out = 1; } out.push(item); }); return [out, globalThis.out]; });
t("block-read", function () { var y = "outer"; function o() { { let y = "inner"; } return y; } return o(); });
t("for-of-shadow", function () { let item = "oi"; const inner = () => { for (let item of [1]) {} return item; }; return inner(); });
t("for-let-shadow", function () { let i = "oi"; const inner = () => { for (let i = 0; i < 1; i++) {} return i; }; return inner(); });
t("switch-shadow", function () { let c = "oc"; const inner = () => { switch (1) { case 1: let c = 2; } return c; }; return inner(); });
t("class-shadow", function () { let k = "ok"; const inner = () => { { class k {} } return k; }; return inner(); });
t("grandchild", function () { let x = 1; const f = () => { { let x = 2; } return () => x; }; return f()(); });
t("read-before-shadow", function () { let x = 1; const f = () => { var y = x; { const x = 3; } return y; }; return f(); });
t("block-fn-annexb", function () { let v = "outer"; function g() { { function v() {} } return typeof v; } return g(); });
// R039: destructuring defaults in a C-style for head.
t("for-head-default", function () { let x = 7; const g = () => { for (let { a = x } = {}; ;) { return a; } }; return g(); });
t("for-head-arrow", function () { let x = 8; let s = 0; for (let { f = () => x } = {}; ;) { s += f(); break; } return s; });
t("for-head-array", function () { let x = 10; const g = () => { for (let [a = x] = []; ;) { return a; } }; return g(); });
t("for-head-var", function () { let x = 11; const g = () => { for (var { a = x } = {}; ;) { return a; } }; return g(); });
// Block-level destructuring leaves bind at block entry.
t("block-destr-captured", function () { { const g = () => p; let { p } = { p: 1 }; return g(); } });
t("globals-untouched", function () { return [factor, x, status, total, gb, out]; });
"##;
const CAPTURE_EXPECTED: &str = r##"
spread-call 5
spread-array [3,6]
spread-new ["z"]
spread-push [7,7]
spread-super [3]
spread-late-write 9
spread-redux [{"id":2}]
update-computed [0,1]
update-target-closure [2,5]
private-in false
private-in-closure false
import-call "function"
param-read 5
param-write [9,"GLOBAL"]
param-shadow-global 5
param-arrow 42
param-method "ABC"
param-self "function"
param-destructured 4
param-rest 2
param-iife 9
param-hot 3000
block-let-shadow [12,100]
catch-shadow ["done","global-status"]
const-shadow [[1,2,3],"global-out"]
block-read "outer"
for-of-shadow "oi"
for-let-shadow "oi"
switch-shadow "oc"
class-shadow "ok"
grandchild 1
read-before-shadow 1
block-fn-annexb "function"
for-head-default 7
for-head-arrow 8
for-head-array 10
for-head-var 11
block-destr-captured 1
globals-untouched [100,"GLOBAL","global-status",100,"global-b","global-out"]
"##;
const TDZ: &str = r##"
function t(name, f) { try { console.log(name + " " + JSON.stringify(f())); } catch (e) { console.log(name + " THROW " + e.name); } }
// Same-named globals: a forward reference must not reach these.
var q = "global", gy = "g", d7 = "global-d7";
// R019: uncaptured function-body lexicals are in their TDZ from entry.
t("strict-write-before", function () { "use strict"; try { q = "clobbered"; } catch (e) { return [e.name, q]; } let q = 1; return "no error"; });
t("typeof-before", function () { try { return typeof c; } catch (e) { return e.name; } let c; });
t("write-before", function () { try { d7 = 1; } catch (e) { return [e.name, d7]; } let d7; return "no error"; });
t("class-typeof-before", function () { try { return typeof K2; } catch (e) { return e.name; } class K2 {} });
t("class-write-before", function () { try { K3 = 1; } catch (e) { return e.name; } class K3 {} return "no error"; });
t("method-before", function () { class K { m() { try { bb = 0; } catch (e) { return e.name; } let bb; return "no error"; } } return new K().m(); });
t("generator-before", function () { function* g() { try { yield typeof w; } catch (e) { yield e.name; } let w = 1; } return [...g()]; });
t("arrow-before", function () { return (() => { var r; try { w; } catch (e) { r = e.name; } let w = 5; return [r, w]; })(); });
t("static-block-before", function () { var o; class A { static { var r; try { w; } catch (e) { r = e.name; } let w = 6; o = [r, w]; } } return o; });
t("body-use-after", function () { var r; try { y; } catch (e) { r = e.name; } let y = 5; y += 1; return [r, y]; });
t("const-after", function () { var r; try { c; } catch (e) { r = e.name; } const c = 7; try { c = 8; } catch (e) { r += "/" + e.name; } return [r, c]; });
t("class-after", function () { var r; try { K; } catch (e) { r = e.name; } class K { static v = 1; } return [r, K.v, typeof K]; });
t("destr-after", function () { var r; try { p; } catch (e) { r = e.name; } let { p, q } = { p: 1, q: 2 }; return [r, p, q]; });
t("multi-declarator", function () { let b2 = "outer"; { try { let a = b2, b2 = 1; return a; } catch (e) { return e.name; } } });
t("closure-after", function () { function f() { const g = () => w; let w = 9; return g(); } return f(); });
t("ok-plain", function () { let a = 1; a = a + 1; { let b = a; a = b * 2; } return a; });
// R030: block lexicals are in their TDZ from block entry.
t("block-read-outer", function () { let y = "outer"; { var r; try { r = y; } catch (e) { return e.name; } let y = 1; } return r; });
t("block-typeof-outer", function () { let y = "outer"; { var r; try { r = typeof y; } catch (e) { return e.name; } let y = 1; } return r; });
t("block-typeof-global", function () { { var r; try { r = typeof gy; } catch (e) { return e.name; } let gy = 1; } return r; });
t("block-class-outer", function () { let y = "outer"; { var r; try { r = y; } catch (e) { return e.name; } class y {} } return r; });
t("block-write-before", function () { { try { e9 = 1; } catch (e) { return e.name; } let e9; } return "no error"; });
t("block-class-after", function () { { var r; try { K; } catch (e) { r = e.name; } class K { static v = 2; } return [r, K.v, new K() instanceof K]; } });
t("block-destr-before", function () { let p = "outer"; { var r; try { r = p; } catch (e) { r = e.name; } let [p1, p] = [5, 6]; return [r, p, p1]; } });
t("block-loop", function () { var o = []; for (var i = 0; i < 3; i++) { var r; try { z; } catch (e) { r = e.name; } let z = i * 2; z++; o.push(r + z); } return o; });
t("block-hot", function () { let n = 0; for (let k = 0; k < 3000; k++) { let y = k; { try { n += y; } catch { n = -1; break; } let y = 2; } } return n; });
t("for-in-head", function () { let k = "outer"; try { for (let k in { a: k }) {} } catch (e) { return e.name; } return "no error"; });
// R280/R022: a switch CaseBlock binding entered past its declaration.
t("sw-read", function () { function sw(n) { switch (n) { case 0: let y = 5, z = 6, w = 7; case 1: return [y, z, w]; } } return sw(1); });
t("sw-read-one", function () { function sw(n) { switch (n) { case 0: let y = 5; case 1: return y; } } return sw(1); });
t("sw-typeof", function () { return ((v = 1) => { switch (v) { case 0: let x = "secret"; break; case 1: return typeof x; } })(); });
t("sw-default", function () { switch (2) { case 0: let x = 1; break; default: return x; } });
t("sw-default-first", function () { function sw(n) { switch (n) { default: return y; case 1: let y = 1; } } return sw(0); });
t("sw-loop", function () { let r = []; for (let i = 0; i < 2; i++) { switch (i) { case 0: let x = "iter" + i; r.push(x); break; case 1: r.push(x); } } return r; });
t("sw-const-read", function () { switch (1) { case 0: const c = 5; case 1: c; return "no error"; } });
t("sw-const-assign", function () { switch (1) { case 0: const y = 1; case 1: y = 2; } });
t("sw-class", function () { function sw(n) { switch (n) { case 0: class K {} case 1: return typeof K; } } return sw(1); });
t("sw-class-cross", function () { function sw(n) { switch (n) { case 0: class K { static v = 4; } case 1: return K.v; } } var r; try { sw(1); } catch (e) { r = e.name; } return [sw(0), r]; });
t("sw-captured-write", function () { var r = []; for (let i = 0; i < 2; i++) { switch (i) { case 0: let q = "a" + i; r.push(() => q); break; case 1: q = "b"; r.push(() => q); } } return r.map(f => f()); });
t("sw-uncaptured-write", function () { switch (1) { case 0: let z = 1; case 1: try { z = 2; return "no error"; } catch (e) { return e.name; } } });
t("sw-selector-ref", function () { switch (3) { case 0: let x = 1; break; case x: return "x"; } return "none"; });
t("sw-selector-closure", function () { function sw(n) { switch (n) { case ({ tag: "T" + n }).tag: let y = 5; default: return y; } } return sw(0); });
t("sw-destr", function () { function sw(n) { switch (n) { case 0: let { y } = { y: 5 }; case 1: return y; } } var r; try { r = sw(1); } catch (e) { r = e.name; } return [sw(0), r]; });
t("sw-fallthrough-ok", function () { function sw(n) { switch (n) { case 0: let y = 5; case 1: return y; } } return sw(0); });
t("sw-same-clause-ok", function () { var o = []; for (let i = 0; i < 3; i++) { switch (i % 2) { case 0: let v = i * 2; o.push(v); break; default: o.push(-1); } } return o; });
t("sw-closure-read", function () { function sw(n) { switch (n) { case 1: return (() => y)(); case 0: let y = 5; } } return sw(1); });
t("globals-untouched", function () { return [q, gy, d7]; });
"##;
const TDZ_EXPECTED: &str = r##"
strict-write-before THROW ReferenceError
typeof-before "ReferenceError"
write-before THROW ReferenceError
class-typeof-before "ReferenceError"
class-write-before "ReferenceError"
method-before "ReferenceError"
generator-before ["ReferenceError"]
arrow-before ["ReferenceError",5]
static-block-before ["ReferenceError",6]
body-use-after ["ReferenceError",6]
const-after ["ReferenceError/TypeError",7]
class-after ["ReferenceError",1,"function"]
destr-after ["ReferenceError",1,2]
multi-declarator "ReferenceError"
closure-after 9
ok-plain 4
block-read-outer "ReferenceError"
block-typeof-outer "ReferenceError"
block-typeof-global "ReferenceError"
block-class-outer "ReferenceError"
block-write-before "ReferenceError"
block-class-after ["ReferenceError",2,true]
block-destr-before ["ReferenceError",6,5]
block-loop ["ReferenceError1","ReferenceError3","ReferenceError5"]
block-hot -1
for-in-head "ReferenceError"
sw-read THROW ReferenceError
sw-read-one THROW ReferenceError
sw-typeof THROW ReferenceError
sw-default THROW ReferenceError
sw-default-first THROW ReferenceError
sw-loop THROW ReferenceError
sw-const-read THROW ReferenceError
sw-const-assign THROW ReferenceError
sw-class THROW ReferenceError
sw-class-cross [4,"ReferenceError"]
sw-captured-write THROW ReferenceError
sw-uncaptured-write "ReferenceError"
sw-selector-ref THROW ReferenceError
sw-selector-closure THROW ReferenceError
sw-destr [5,"ReferenceError"]
sw-fallthrough-ok 5
sw-same-clause-ok [0,-1,4]
sw-closure-read THROW ReferenceError
globals-untouched ["global","g","global-d7"]
"##;
const LABELS: &str = r##"
function t(name, f) { try { console.log(name + " " + JSON.stringify(f())); } catch (e) { console.log(name + " THROW " + e.name); } }
// R007: `continue L` for any label of a loop's label set.
t("label-set-for", function () { var n = 0; outer: inner: for (var i = 0; i < 3; i++) { if (i === 1) continue outer; n += i; } return n; });
t("label-set-three", function () { var m = 0; x: y: z: for (var j = 0; j < 3; j++) { if (j === 1) continue x; if (j == 2) continue z; m += j; } return m; });
t("label-set-while", function () { var k = 0; a: b: c: d: while (k < 5) { k++; continue b; } return k; });
t("label-set-for-of", function () { var s = 0; o: p: for (var q of [1, 2, 3]) { if (q === 2) continue o; s += q; } return s; });
t("label-set-do", function () { return new Function("var r = 0; o: p: do { r++; continue o; } while (false); return r;")(); });
t("label-set-eval", function () { var s2 = 0; eval("o: p: for (var q of [1, 2, 3]) { if (q === 2) continue o; s2 += q; }"); return s2; });
t("label-set-nested", function () { var r = []; a: b: for (var i = 0; i < 2; i++) { c: { if (i == 0) continue a; r.push(i); break c; } } return r; });
t("label-set-break", function () { var r = 0; a: b: for (;;) { r++; break a; } return r; });
t("label-set-close", function () { var log = []; var it = { [Symbol.iterator]() { var i = 0; return { next() { return { done: i >= 3, value: i++ }; }, return() { log.push("ret"); return {}; } }; } }; outer: mid: for (var v of it) { for (var w of it) { continue outer; } } return log; });
// R021/R052/R162: a labelled block is an ordinary block.
t("lbl-fn-hoist", function () { L: { var r = f(); function f() { return "hoisted"; } } return r; });
t("lbl-fn-strict", function () { "use strict"; L: { var r = f(); function f() { return "hoisted"; } } return r; });
t("lbl-closure-let", function () { L: { function h() { return x; } let x = "block"; return h(); } });
t("lbl-tdz", function () { L: { try { x; } catch (e) { return e.name; } let x = 1; } });
t("lbl-using", function () { const log = []; L: { using r = { [Symbol.dispose]() { log.push("disposed"); } }; log.push("body"); } log.push("after"); return log.join(); });
t("lbl-using-break", function () { const log = []; L: { using r = { [Symbol.dispose]() { log.push("disposed"); } }; log.push("body"); if (true) break L; log.push("no"); } log.push("after"); return log.join(); });
t("lbl-using-throw", function () { const log = []; try { M: { using z2 = { [Symbol.dispose]() { log.push("dthrow"); } }; throw new Error("boom"); } } catch (e) { log.push("caught"); } return log.join(); });
t("lbl-using-nested", function () { const log = []; const res = n => ({ [Symbol.dispose]() { log.push("d" + n); } }); (function () { using a = res("outer"); L: { using b = res("inner"); log.push("body"); } log.push("mid"); })(); return log.join(); });
t("lbl-using-gen", function () { const log = []; function* g() { lb: { using y = { [Symbol.dispose]() { log.push("dgen"); } }; yield 1; break lb; } log.push("after"); } for (const _ of g()); return log.join(); });
// The same lowering for an arrow's body: its top-level `using` is disposed.
t("arrow-using", function () { const log = []; const r = (() => { using a = { [Symbol.dispose]() { log.push("disposed"); } }; log.push("body"); return 5; })(); return [r, log.join()]; });
t("arrow-using-throw", function () { const log = []; try { (() => { using a = { [Symbol.dispose]() { log.push("disposed"); } }; throw new Error("x"); })(); } catch (e) { log.push("caught"); } return log.join(); });
t("lbl-using-await", function () { const log = []; (async function () { L: { await using a = { async [Symbol.asyncDispose]() { log.push("adisp"); } }; log.push("body"); } log.push("after"); })(); return log.join(); });
// R029: a typeof fact of the `then` arm does not hold in the `else` arm.
t("typeof-else", function () { function f(c) { var v; if (c) var t = typeof v; else return t === "undefined"; return "x"; } return [f(false), f(true)]; });
t("typeof-else-block", function () { function g(c) { var v = 1; if (c) var t = typeof v; else { return [t === "number", t == "number", t !== "number"]; } } return g(false); });
t("typeof-still", function () { function h(c) { var v = 1; var t = typeof v; if (c) { v = "s"; } else { return t === "number"; } return t === "number"; } return [h(false), h(true)]; });
"##;
const LABELS_EXPECTED: &str = r##"
label-set-for 2
label-set-three 0
label-set-while 5
label-set-for-of 4
label-set-do 1
label-set-eval 4
label-set-nested [1]
label-set-break 1
label-set-close ["ret","ret","ret"]
lbl-fn-hoist "hoisted"
lbl-fn-strict "hoisted"
lbl-closure-let "block"
lbl-tdz "ReferenceError"
lbl-using "body,disposed,after"
lbl-using-break "body,disposed,after"
lbl-using-throw "dthrow,caught"
lbl-using-nested "body,dinner,mid,douter"
lbl-using-gen "dgen,after"
arrow-using [5,"body,disposed"]
arrow-using-throw "disposed,caught"
lbl-using-await "body,adisp"
typeof-else [false,"x"]
typeof-else-block [false,false,true]
typeof-still [true,true]
"##;
const CLASSES: &str = r##"
function t(name, f) { try { console.log(name + " " + JSON.stringify(f())); } catch (e) { console.log(name + " THROW " + e.name); } }
// R018: static fields are DefineField, not [[Set]].
t("static-name", function () { class A { static name = "custom"; } return [A.name, Object.getOwnPropertyDescriptor(A, "name")]; });
t("static-length", function () { class A { static length = 42; } return A.length; });
t("static-name-strict", function () { "use strict"; class A { static name = "custom"; } return A.name; });
t("inherited-setter", function () { var log = []; class B { static set x(v) { log.push("setter " + v); } } class D extends B { static x = 1; } return [log, Object.hasOwn(D, "x"), D.x]; });
t("computed-name", function () { class C { static ["name"] = "computed"; } return C.name; });
t("computed-inherited-setter", function () { var log = []; class B { static set y(v) { log.push(v); } } var k = "y"; class D extends B { static [k] = 2; } return [log, Object.hasOwn(D, "y")]; });
t("getter-shadow", function () { class C { static get y() { return "getter"; } static y = "field"; } return C.y; });
t("frozen-parent", function () { class B {} Object.defineProperty(B, "q", { value: 1, writable: false }); class D extends B { static q = 2; } return [D.q, Object.hasOwn(D, "q")]; });
t("plain-static", function () { class A { static z = 1; } return Object.getOwnPropertyDescriptor(A, "z"); });
t("symbol-static", function () { var s = Symbol("k"); class A { static [s] = 5; static [Symbol.iterator] = 1; } return [A[s], Object.getOwnPropertySymbols(A).length, Object.getOwnPropertyNames(A).filter(n => n.indexOf("Symbol") >= 0)]; });
t("numeric-static", function () { class A { static [1] = 5; static 2 = 6; } return [A[1], A[2], Object.keys(A)]; });
t("private-static", function () { class A { static #p = 3; static g() { return A.#p; } } return A.g(); });
t("prototype-key", function () { var k = "prototype"; class A { static [k] = 1; } return 1; });
t("order", function () { var log = []; class A { static a = log.push("a"); static { log.push("blk"); } static ["b"] = log.push("b"); } return [log, Object.keys(A)]; });
t("this-field", function () { class A { static a = 1; static b = this.a + 1; } return A.b; });
t("anon-names", function () { class A { static f = function () {}; static ["g"] = () => 1; } return [A.f.name, A.g.name]; });
t("anon-class-static-name", function () { const C = class { static name = "x"; }; return C.name; });
t("non-extensible", function () { class C { static a = Object.preventExtensions(this); static b = 1; } return C.b; });
t("computed-non-extensible", function () { var k = "b"; class C { static a = Object.preventExtensions(this); static [k] = 1; } return C.b; });
t("frozen-name", function () { class C { static a = Object.freeze(this); static name = "x"; } return C.name; });
t("setter-replaced", function () { class C { static set v(x) { throw 1; } static v = 5; } return Object.getOwnPropertyDescriptor(C, "v"); });
// R276: a static block that uses an enclosing function's local.
t("static-block-let", function () { let r = 0; class A { static { r = 1; } } return r; });
t("static-block-var", function () { var r = "a"; class A { static { r += "b"; } } return r; });
t("static-block-read", function () { const base = 40; let out; class A { static { out = base + 2; } } return out; });
t("static-block-arrow-iife", function () { return (() => { let reg = new Map(); class A { static { reg.set("A", A); } } return reg.get("A") === A; })(); });
t("static-block-hot", function () { let n = 0; for (let i = 0; i < 200; i++) { class A { static { n++; } } } return n; });
"##;
const CLASSES_EXPECTED: &str = r##"
static-name ["custom",{"value":"custom","writable":true,"enumerable":true,"configurable":true}]
static-length 42
static-name-strict "custom"
inherited-setter [[],true,1]
computed-name "computed"
computed-inherited-setter [[],true]
getter-shadow "field"
frozen-parent [2,true]
plain-static {"value":1,"writable":true,"enumerable":true,"configurable":true}
symbol-static [5,2,[]]
numeric-static [5,6,["1","2"]]
private-static 3
prototype-key THROW TypeError
order [["a","blk","b"],["a","b"]]
this-field 2
anon-names ["f","g"]
anon-class-static-name "x"
non-extensible THROW TypeError
computed-non-extensible THROW TypeError
frozen-name THROW TypeError
setter-replaced {"value":5,"writable":true,"enumerable":true,"configurable":true}
static-block-let 1
static-block-var "ab"
static-block-read 42
static-block-arrow-iife true
static-block-hot 200
"##;
const PARAM_SCOPE: &str = r##"
function t(name, f) { try { console.log(name + " " + JSON.stringify(f())); } catch (e) { console.log(name + " THROW " + e.name); } }
// R023: parameter expressions give the body a separate var environment.
t("var-redecl", function () { function f(x = 1, g = () => x) { var x = 2; var h = () => x; return g() + "," + h(); } return f(); });
t("var-redecl-read", function () { function f(x = 1, g = () => x) { var x = 2; return g() + "," + x; } return f(); });
t("bare-var", function () { function f(x = 1, g = () => x) { var x; return g() + "," + x; } return f(5); });
t("param-closure-writes", function () { function f(x = 1, set = v => { x = v; }, get = () => x) { var x = 2; set(9); return [get(), x]; } return f(); });
t("fn-decl", function () { function f(x = 1, g = () => x) { function x() {} return [g(), typeof x]; } return f(); });
t("arrow", function () { return ((x = 1, g = () => x) => { var x = 2; return [g(), x]; })(); });
t("method", function () { class C { m(x = 1, g = () => x) { var x = 2; return [g(), x]; } } return new C().m(); });
t("strict", function () { "use strict"; function f(x = 1, g = () => x) { var x = 2; return [g(), x]; } return f(); });
t("destructured", function () { function f({ x } = { x: 1 }, g = () => x) { var x = 2; return [g(), x]; } return f(); });
t("generator", function () { function* f(x = 1, g = () => x) { var x = 2; yield g(); yield x; } return [...f()]; });
t("body-closure-writes", function () { function f(x = 1, g = () => x) { var x = 2; var set = v => { x = v; }; set(7); return [g(), x]; } return f(); });
t("hot", function () { function f(x, g = () => x) { var x = x + 1; return g() + x; } var s = 0; for (var i = 0; i < 3000; i++) s += f(i); return s; });
t("simple-shared", function () { function f(x) { var x; return x; } return f(3); });
t("no-closure-shared", function () { function f(x = 1) { var x; return x; } return f(4); });
// The body copy is taken at call time, before a generator suspends.
t("gen-prologue", function () { var leak; function* g(x = 1, s = (leak = v => { x = v; })) { var x; yield x; } const it = g(); leak(5); return it.next().value; });
t("gen-shared", function () { var leak; function* h(x = 1, s = (leak = v => { x = v; })) { yield x; } const it = h(); leak(7); return it.next().value; });
t("method-gen-prologue", function () { var leak; class C { *g(x = 1, s = (leak = v => { x = v; })) { var x; yield x; } } const it = new C().g(); leak(5); return it.next().value; });
"##;
const PARAM_SCOPE_EXPECTED: &str = r##"
var-redecl "1,2"
var-redecl-read "1,2"
bare-var "5,5"
param-closure-writes [9,2]
fn-decl [1,"function"]
arrow [1,2]
method [1,2]
strict [1,2]
destructured [1,2]
generator [1,2]
body-closure-writes [1,7]
hot 9000000
simple-shared 3
no-closure-shared 4
gen-prologue 1
gen-shared 7
method-gen-prologue 1
"##;
const DECORATORS: &str = r##"
function t(name, f) { try { console.log(name + " " + JSON.stringify(f())); } catch (e) { console.log(name + " THROW " + e.name); } }
// R161: closures inside decorator expressions capture enclosing locals.
t("class-dec-write", function () { let out = "none"; @((v, c) => { out = "set"; }) class C {} return [out, typeof globalThis.out]; });
t("method-dec-write", function () { let x = 0; class C { @((v, c) => { x = 1; }) m() {} } return x; });
t("static-field-dec", function () { let y = 0; class C { @((v, c) => { y = 3; }) static fld = 1; } return y; });
t("getter-dec", function () { let w = 0; class C { @(function (v, c) { w = 5; }) get g() { return 1; } } return w; });
t("factory-dec", function () { function mk(fn) { return (v, c) => { fn(); }; } let n = 0; const C = @mk(() => { n++; }) class {}; return n; });
t("accessor-dec", function () { let a = 0; class C { @((v, c) => { a = 7; return v; }) accessor z = 1; } return a; });
t("read-dec", function () { let r; let z = 7; @((v, c) => { r = z; }) class C {} return r; });
"##;
const DECORATORS_EXPECTED: &str = r##"
class-dec-write ["set","undefined"]
method-dec-write 1
static-field-dec 3
getter-dec 5
factory-dec 1
accessor-dec 7
read-dec 7
"##;
const AWAIT_USING: &str = r##"
// R167: `await using` disposal awaits only what DisposeResources awaits.
var lines = [];
function ticker(o, n) { return (async () => { for (let i = 0; i < n; i++) { o.push("t" + i); await null; } })(); }
async function nulls2() { const o = []; const t = ticker(o, 4); { await using a = null, b = null; o.push("body"); } o.push("after"); await t; return o.join(); }
async function null1() { const o = []; const t = ticker(o, 4); { await using a = null; o.push("body"); } o.push("after"); await t; return o.join(); }
async function nullThenAsync() { const o = []; const t = ticker(o, 5); { await using a = { async [Symbol.asyncDispose]() { o.push("dispose"); } }, b = null; o.push("body"); } o.push("after"); await t; return o.join(); }
async function syncInAsyncScope() { const o = []; const t = ticker(o, 5); { using s = { [Symbol.dispose]() { o.push("sync"); return Promise.resolve(); } }; await using a = { async [Symbol.asyncDispose]() { o.push("async"); } }; o.push("body"); } o.push("after"); await t; return o.join(); }
async function nullBeforeSync() { const o = []; const t = ticker(o, 5); { using s = { [Symbol.dispose]() { o.push("sync"); } }; await using a = null; o.push("body"); } o.push("after"); await t; return o.join(); }
async function syncFallback() { const o = []; const t = ticker(o, 5); { await using a = { [Symbol.dispose]() { o.push("fallback"); } }; o.push("body"); } o.push("after"); await t; return o.join(); }
async function throwing() { const o = []; try { { await using a = null, b = { async [Symbol.asyncDispose]() { throw new Error("B"); } }, c = { [Symbol.dispose]() { throw new Error("C"); } }; o.push("body"); } } catch (e) { o.push(e.constructor.name + ":" + e.error.message + "/" + e.suppressed.message); } return o.join(); }
async function asyncArrowBody() { const o = []; await (async () => { await using a = { async [Symbol.asyncDispose]() { await null; await null; o.push("disposed"); } }; o.push("body"); })(); o.push("after"); return o.join(); }
async function asyncArrowNull() { const o = []; const t = ticker(o, 6); await (async () => { await using a = null; o.push("body"); })(); o.push("after"); await t; return o.join(); }
async function empty() { const o = []; const t = ticker(o, 3); for (let i = 0; i < 1; i++) { if (i === 0) break; await using a = null; } o.push("after"); await t; return o.join(); }
(async () => {
  for (const f of [nulls2, null1, nullThenAsync, syncInAsyncScope, nullBeforeSync, syncFallback, throwing, asyncArrowBody, asyncArrowNull, empty]) {
    lines.push(f.name + " " + await f());
  }
  console.log(lines.join("\n"));
})();
"##;
const AWAIT_USING_EXPECTED: &str = r##"
nulls2 t0,body,t1,after,t2,t3
null1 t0,body,t1,after,t2,t3
nullThenAsync t0,body,dispose,t1,after,t2,t3,t4
syncInAsyncScope t0,body,async,t1,sync,after,t2,t3,t4
nullBeforeSync t0,body,t1,sync,after,t2,t3,t4
syncFallback t0,body,fallback,t1,after,t2,t3,t4
throwing body,SuppressedError:B/C
asyncArrowBody body,disposed,after
asyncArrowNull t0,body,t1,t2,after,t3,t4,t5
empty t0,after,t1,t2
"##;
const MODULE: &str = r##"
// R018: a module is strict, so a [[Set]] of the read-only `name` threw.
class Widget { static name = "my-widget"; static length = 3; }
console.log("static-name " + Widget.name + " " + Widget.length);
class W2 { static ["name"] = "computed"; }
console.log("static-computed " + W2.name);
// R276: a static block over a module function's local.
export function sb() { let r = 0; class A { static { r = 1; } } return r; }
console.log("static-block " + sb());
// R034: the lazy-load idiom captures its specifier.
function lazy() { const spec = "./zipp-audit-missing-module.mjs"; return () => import(spec); }
const pending = lazy()();
pending.catch(() => {});
console.log("import-call " + typeof pending.then);
// R016: a default's closure over a parameter, hot.
export function f(value, cmp = (o) => o === value) { return cmp(value); }
let hits = 0;
for (let i = 0; i < 3000; i++) if (f(i)) hits++;
console.log("param-default " + hits);
// R017/R121: block shadowing and spread operands.
function blk() { let x = 1; const g = () => { { let x = 2; } return x; }; return g(); }
console.log("block-shadow " + blk());
function k3() { const k = 3; return Math.max(...[1, 2].map(v => v * k)); }
console.log("spread " + k3());
// R019: a write before `let` is a TDZ error, not a global store.
function g() { try { q = 1; } catch (e) { return e.name; } let q; return "no error"; }
console.log("tdz " + g() + " " + typeof globalThis.q);
"##;
const MODULE_EXPECTED: &str = r##"
static-name my-widget 3
static-computed computed
static-block 1
import-call function
param-default 3000
block-shadow 1
spread 6
tdz ReferenceError undefined
"##;

#[test]
fn compile_audit_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    check_script("assign_target", ASSIGN_TARGET, ASSIGN_TARGET_EXPECTED);
    check_script("eval_order", EVAL_ORDER, EVAL_ORDER_EXPECTED);
    check_script("capture", CAPTURE, CAPTURE_EXPECTED);
    check_script("tdz", TDZ, TDZ_EXPECTED);
    check_script("labels", LABELS, LABELS_EXPECTED);
    check_script("classes", CLASSES, CLASSES_EXPECTED);
    check_script("param_scope", PARAM_SCOPE, PARAM_SCOPE_EXPECTED);
    check_script("decorators", DECORATORS, DECORATORS_EXPECTED);
    check_script("await_using", AWAIT_USING, AWAIT_USING_EXPECTED);
    check_module("module", MODULE, MODULE_EXPECTED);
}

#[test]
fn compile_audit_modes_match() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("default", None),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "compile_audit_child", "--nocapture"])
            // An inherited setting must not decide what a mode tests.
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env(CHILD_ENV, "1");
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// R020: the bare `Math.fn()` lowering carried `Math`'s global slot in a
/// register field that `check_regs` renumbers from 0x8000 up, so with more
/// than 32,768 leading globals a function that also used a boolean register
/// called a method of an unrelated global.
#[test]
fn bare_math_call_survives_a_global_slot_above_the_register_classes() {
    for (label, decoy) in [("plain", false), ("decoy", true)] {
        let mut src = String::new();
        for i in 0..33_000 {
            if decoy {
                src.push_str(&format!("var g{i} = {{ abs() {{ return 'WRONG'; }} }};"));
            } else {
                src.push_str(&format!("var g{i};"));
            }
        }
        src.push_str(
            "\nvar src = 'hello';\
             function f(a) { var t = a < 0; var c = src.charCodeAt(0); return Math.abs(a); }\
             var s = 0; for (var i = 0; i < 2000; i++) s += f(-2);\
             console.log(f(-2), s);",
        );
        let out = zipp_vm::run(&src).unwrap_or_else(|e| panic!("{label}: compile failed: {e}"));
        assert_eq!(out.error, None, "{label}");
        assert_eq!(out.output, ["2 4000"], "{label}");
    }
}

/// R024/R041: `ObjectRest.exclude_count` is a u16. Counting siblings into it
/// unchecked wrapped at 65,536 and leaked the excluded keys into the rest
/// object; a wider pattern is now a SyntaxError, and the widest legal one
/// still excludes every sibling.
#[test]
fn object_rest_sibling_count_cannot_wrap() {
    let keys = |n: usize, target: &str| {
        (0..n)
            .map(|i| format!("k{i}: {target}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    for (label, src) in [
        (
            "assignment",
            format!(
                "function f(o) {{ var t = {{}}; ({{{}, ...t.r}} = o); return t; }}",
                keys(65_536, "t.a")
            ),
        ),
        (
            "declaration",
            format!(
                "function f(o) {{ var {{{}, ...r}} = o; return r; }}",
                keys(65_536, "a")
            ),
        ),
    ] {
        let error = match zipp_vm::run(&src) {
            Err(error) => error,
            Ok(_) => panic!("{label}: 65,536 rest siblings cannot fit ObjectRest metadata"),
        };
        assert!(
            error.contains("too many sibling properties"),
            "{label}: {error}"
        );
    }
    let src = format!(
        "function f(o) {{ var t = {{}}; ({{{}, ...t.r}} = o); return t; }}\
         console.log(JSON.stringify(f({{k0: 1, k65534: 2, zz: 3}}).r));",
        keys(65_535, "t.a")
    );
    let out = zipp_vm::run(&src).expect("65,535 rest siblings compile");
    assert_eq!(out.error, None);
    assert_eq!(out.output, [r#"{"zz":3}"#]);
}

/// R025: a literal too long for one `NewArray` block builds incrementally
/// even without a spread, and must still read the target's old value.
#[test]
fn long_array_literal_reads_its_target_before_replacing_it() {
    let mut src = String::from("function f() { let a = [7]; a = [a[0]");
    for _ in 0..1_100 {
        src.push_str(", 0");
    }
    src.push_str("]; return a[0] + ':' + a.length; } console.log(f());");
    let out = zipp_vm::run(&src).expect("source compiles");
    assert_eq!(out.error, None);
    assert_eq!(out.output, ["7:1101"]);
}
