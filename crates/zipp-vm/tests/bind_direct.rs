//! `f.bind(t, ...args)` on a plain function or native makes its bound
//! function directly while a Get of `bind` finds the pristine
//! `%Function.prototype.bind%`, and `Array.prototype.values.call(arr)` makes
//! the array's live iterator directly (`interp_fn_bind_fast`,
//! `interp_array_values_call`): the two halves of a loose-mode for...of's
//! setup. Every `name`, `length` and prototype a bound function reports, a
//! bound function of a bound function, `new` and `instanceof` through one,
//! an own `name`, `length` or `bind`, a class target, a generator's and an
//! async function's prototype, a replaced, accessor or deleted
//! `Function.prototype.bind` or `.call`, and `values` on holes, a mutated
//! array, `arguments`, an array-like and a string must behave as before.
//! The expected lines are Node's.

const SOURCE: &str = r##"
"use strict";
// Behaviours the direct bind and Array.prototype.values.call paths must keep.
const out = [];
const log = (...a) => out.push(a.map((x) => typeof x === "string" ? x : String(x)).join(" "));
function heat(f) { let r; for (let i = 0; i < 300; i++) r = f(i); return r; }

function f(a, b, c) { return [this && this.tag, a, b, c].join(","); }
log("bound name/len", heat(() => { const b = f.bind({ tag: "T" }, 1); return b.name + "/" + b.length + "/" + b(2, 3); }));
log("bound no args", heat(() => { const b = f.bind(); return b.name + "/" + b.length + "/" + typeof b; }));
log("bound many", heat(() => f.bind(null, 1, 2, 3, 4).length));
const nx = [][Symbol.iterator]().next;
log("native next bound", heat(() => { const b = nx.bind([5, 6][Symbol.iterator]()); return b.name + "/" + b.length + "/" + JSON.stringify(b()); }));
log("bound of bound", heat(() => { const b = f.bind(null, 1).bind(null, 2); return b.name + "/" + b.length + "/" + b(3); }));
function* gen() { yield 1; }
log("gen proto", heat(() => Object.getPrototypeOf(gen.bind(null)) === Object.getPrototypeOf(gen)));
async function af() {}
log("async proto", heat(() => Object.getPrototypeOf(af.bind(null)) === Object.getPrototypeOf(af)));
const g = function g2() {};
Object.setPrototypeOf(g, { bind: Function.prototype.bind, marker: 1 });
log("custom proto", heat(() => { const b = g.bind(null); return Object.getPrototypeOf(b) === Object.getPrototypeOf(g); }));
const named = function n0() {};
Object.defineProperty(named, "name", { value: "renamed" });
log("own name", heat(() => named.bind(null).name));
const lenf = function (a, b) {};
Object.defineProperty(lenf, "length", { value: 7 });
log("own length", heat(() => lenf.bind(null, 1).length));
const ownBind = function () {};
ownBind.bind = function () { return "own bind"; };
log("own bind", heat(() => ownBind.bind(null)));
class K { m() {} }
log("class bind", heat(() => { const b = K.bind(null); return b.name + "/" + (new b() instanceof K); }));
log("bound new", heat(() => { function P(x) { this.x = x; } const B = P.bind(null, 9); return new B().x; }));
log("instanceof", heat(() => { function Q() {} const B = Q.bind(null); return new Q() instanceof B; }));
log("bound this primitive", heat(() => (function () { return typeof this; }).bind(5)()));

// Array.prototype.values.call
const vals = Array.prototype[Symbol.iterator];
log("values ident", vals === Array.prototype.values);
log("values call", heat(() => JSON.stringify([...{ [Symbol.iterator]: () => vals.call([1, , 3]) }])));
log("values live", heat(() => { const a = [1, 2]; const it = vals.call(a); a.push(3); let s = 0; for (let r = it.next(); !r.done; r = it.next()) s += r.value; return s; }));
log("values args", heat(() => (function () { const it = vals.call(arguments); arguments[0] = 10; return it.next().value; })(1, 2)));
log("values obj", heat(() => { const it = vals.call({ length: 2, 0: "a", 1: "b" }); return it.next().value + it.next().value; }));
log("values str", heat(() => { try { return vals.call("xy").next().value; } catch (e) { return e.constructor.name; } }));
log("values proto", heat(() => Object.getPrototypeOf(vals.call([])) === Object.getPrototypeOf([][Symbol.iterator]())));
log("keys call", heat(() => Array.prototype.keys.call([7, 8]).next().value));
log("entries call", heat(() => JSON.stringify(Array.prototype.entries.call([7, 8]).next().value)));

// The loose helper end to end.
function _createForOfIteratorHelperLoose(r, e) {
  var t2 = "undefined" != typeof Symbol && r[Symbol.iterator] || r["@@iterator"];
  if (t2) return (t2 = t2.call(r)).next.bind(t2);
  throw new TypeError("x");
}
log("helper", heat(() => { let s = 0; for (var it = _createForOfIteratorHelperLoose([1, 2, 3]), st; !(st = it()).done;) s += st.value; return s; }));
log("helper set", heat(() => { let s = 0; for (var it = _createForOfIteratorHelperLoose(new Set([4, 5])), st; !(st = it()).done;) s += st.value; return s; }));
log("helper map", heat(() => { let s = ""; for (var it = _createForOfIteratorHelperLoose(new Map([["a", 1]])), st; !(st = it()).done;) s += st.value; return s; }));
log("helper str", heat(() => { let s = ""; for (var it = _createForOfIteratorHelperLoose("ab"), st; !(st = it()).done;) s += st.value; return s; }));

// Replacing the intrinsics turns the fast paths off.
const origBind = Function.prototype.bind;
Function.prototype.bind = function () { return "replaced bind"; };
log("replaced bind", heat(() => f.bind(null)));
Function.prototype.bind = origBind;
log("restored bind", heat(() => typeof f.bind(null)));
const origCall = Function.prototype.call;
Function.prototype.call = function () { return "replaced call"; };
log("replaced call", heat(() => vals.call([1])));
Function.prototype.call = origCall;
log("restored call", heat(() => vals.call([1]).next().value));
Object.defineProperty(Function.prototype, "bind", { get() { return () => "getter bind"; }, configurable: true });
log("accessor bind", heat(() => f.bind(null)));
Object.defineProperty(Function.prototype, "bind", { value: origBind, writable: true, configurable: true });
log("back", heat(() => f.bind(null, 1)(2, 3)));
delete Function.prototype.bind;
log("deleted bind", heat(() => { try { return f.bind(null); } catch (e) { return e.constructor.name; } }));
Function.prototype.bind = origBind;
console.log(out.join("\n"));
"##;

fn node_output(source: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source)
        .output()
        .expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn bind_and_values_call_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
