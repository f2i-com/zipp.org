//! Calls whose method name has a builtin lane, on receivers the lane does not
//! serve: user classes, subclasses, own shadows, replaced prototype methods,
//! getters, throwing methods and non-callables, in hot loops. Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"40507499 6000
36000
84000
12000
19500
9003000
x!set
204000
9018000
12000
TypeError
TypeError
3000
15000
3000
2004000
12517500"####;

const PROGRAM: &str = r####"// Method names with a builtin lane, on receivers the lane does not serve, in hot loops.
class Store {
  constructor() { this.d = {}; this.n = 0; }
  get(k) { this.n++; return this.d[k]; }
  set(k, v) { this.d[k] = v; return this; }
  has(k) { return k in this.d; }
  add(k) { this.n += k; return this; }
  delete(k) { delete this.d[k]; return true; }
  clear() { this.d = {}; }
  slice(a, b) { return a + b + this.n; }
  substring(a) { return a * 2; }
  indexOf(x) { return x + 1; }
  getUint8(p) { return p + 3; }
  getFloat64(p, le) { return le ? p : -p; }
}
var s = new Store();
function own(n) { var r = 0; for (var i = 0; i < n; i++) { s.set("k" + (i % 5), i); r += s.get("k" + (i % 5)) + (s.has("k1") ? 1 : 0) + s.slice(i, 1) + s.substring(i) + s.indexOf(i) + s.getUint8(i) + s.getFloat64(i, true); s.add(1); if (i % 100 === 0) { s.delete("k1"); s.clear(); } } return r; }
console.log(own(3000), s.n);
// One site, several receivers; a map and a class, a string and an array, in turn.
var mp = new Map([["a", 1]]), st = new Set([1]), wm = new WeakMap(), ky = {}; wm.set(ky, 9);
function look(o, k) { return o.get(k); }
function chk(o, k) { return o.has(k); }
function mix(n) { var r = 0; for (var i = 0; i < n; i++) { r += look(mp, "a") + look(s, "zz") === undefined ? 0 : 1; r += chk(i % 2 ? mp : st, i % 2 ? "a" : 1) ? 1 : 0; r += wm.get(ky) + (wm.has(ky) ? 1 : 0); } return r; }
console.log(mix(3000));
function slc(x, i) { var v = x.slice(i, i + 2); return typeof v === "string" ? v.length : v.length + 10; }
function sl(n) { var a = [1, 2, 3, 4, 5], b = "héllo wörld", c = "plain ascii", t = new Uint8Array(6), r = 0; for (var i = 0; i < n; i++) { r += slc(a, i % 4) + slc(b, i % 4) + slc(c, i % 4) + slc(t, i % 4); } return r; }
console.log(sl(3000));
function sub(x, i) { return x.substring(i, i + 2); }
function sb(n) { var r = 0; for (var i = 0; i < n; i++) { r += sub("héllo", i % 3).length + sub("hello", i % 3).length; } return r; }
console.log(sb(3000));
function idx(x, v) { return x.indexOf(v); }
function ix(n) { var a = [1, 2, 3], ta = new Int8Array([4, 5]), r = 0; for (var i = 0; i < n; i++) { r += idx(a, i % 4) + idx("abc", "c") + idx("héc", "c") + idx(ta, 5) + idx("a1b", 1); } return r; }
console.log(ix(3000));
// Observable: a Map subclass, an own shadow, a replaced prototype method, a getter.
class M2 extends Map { get(k) { return super.get(k) + 1; } set(k, v) { return super.set(k, v * 2); } }
var m2 = new M2(); m2.set("a", 1);
function sub2(n) { var r = 0; for (var i = 0; i < n; i++) { m2.set("a", i); r += m2.get("a") + (m2.has("a") ? 1 : 0); } return r; }
console.log(sub2(3000));
var own2 = new Map(); own2.get = (k) => k + "!"; own2.set = () => "set";
function shad(n) { var r = ""; for (var i = 0; i < n; i++) { r = own2.get("x") + own2.set(1, 2); } return r; }
console.log(shad(3000));
var realGet = Map.prototype.get;
function rep(n) { var m = new Map([[1, 2]]), r = 0; for (var i = 0; i < n; i++) { if (i === n >> 1) Map.prototype.get = function () { return 100; }; r += m.get(1); } Map.prototype.get = realGet; return r; }
console.log(rep(4000));
var acc = { get get() { return () => 7; }, get slice() { return (a, b) => a * b; } };
function ga(n) { var r = 0; for (var i = 0; i < n; i++) r += acc.get("x") + acc.slice(i, 2); return r; }
console.log(ga(3000));
// A throw from a method in each lane's place, and non-callables.
var bad = { get(k) { throw new RangeError("get" + k); }, slice: 5, add: null, indexOf: undefined };
function tr(n) { var r = 0; for (var i = 0; i < n; i++) { try { bad.get(i); } catch (e) { r += e.name === "RangeError" ? 1 : 0; } try { bad.slice(1, 2); } catch (e) { r += e.name === "TypeError" ? 1 : 0; } try { bad.add(1); } catch (e) { r += e.name === "TypeError" ? 1 : 0; } try { bad.indexOf(1); } catch (e) { r += e.name === "TypeError" ? 1 : 0; } } return r; }
console.log(tr(3000));
try { null.get(1); } catch (e) { console.log(e.name); }
try { undefined.slice(1, 2); } catch (e) { console.log(e.name); }
function pr(x) { return x.slice(1, 2); }
function prm(n) { var r = 0; for (var i = 0; i < n; i++) { try { pr(5); } catch (e) { r++; } } return r; }
console.log(prm(3000));
// DataView reads: a real one, one with its method replaced, off the end.
var dv = new DataView(new ArrayBuffer(8)); dv.setUint16(0, 258);
function rv(d, i) { return d.getUint8(i); }
function dvl(n) { var r = 0; for (var i = 0; i < n; i++) { r += rv(dv, i % 2) + rv(s, i % 2); } return r; }
console.log(dvl(3000));
function oob(n) { var r = 0; for (var i = 0; i < n; i++) { try { rv(dv, 100); } catch (e) { r += e.name === "RangeError" ? 1 : 0; } } return r; }
console.log(oob(3000));
var realU8 = DataView.prototype.getUint8;
function dvr(n) { var r = 0; for (var i = 0; i < n; i++) { if (i === n >> 1) DataView.prototype.getUint8 = () => 1000; r += dv.getUint8(1); } DataView.prototype.getUint8 = realU8; return r; }
console.log(dvr(4000));
// Allocation in the general route under the pinned tables.
var pinned = new Uint32Array(4);
function pa(n) { var r = 0; for (var i = 0; i < n; i++) { pinned[i & 3] = i; r += pinned[i & 3] + [1, 2, 3].slice(0, 2).length + "héllo".slice(0, 2).length; } return r; }
console.log(pa(5000));
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
