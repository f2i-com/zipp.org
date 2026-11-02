//! Tier O's method intrinsics (charCodeAt, indexOf, substring/slice,
//! toUpperCase, Map/Set get/has/set/add/delete/clear, push/pop) and their
//! declines for user methods of the same names, string compares at a site
//! that deopted, class values fixed and evaluated again, and >>> by
//! constants, against Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"900THETA 229000 72800 alpha,beta,delta,epsilon,eta,gamma,theta,zeta1,3,5,7,9 a,b,1,3 NaN,1,2 2200 4 201 true true 284493055 0"####;

const PROGRAM: &str = r####"// Tier O: method intrinsics and their declines, string compares at a site
// that deopted, class values fixed and evaluated again, >>> by constants.
const out = [];
function strs(words) {
  let t = 0, s = "";
  for (let i = 0; i < words.length; i++) {
    const w = words[i];
    t += w.charCodeAt(0) + w.indexOf("a") + w.substring(1, 3).length + w.slice(1).length;
    s = w.toUpperCase();
  }
  return t + s;
}
const words = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"];
let st = "";
for (let r = 0; r < 300; r++) st = strs(words);
out.push(st);
function colls(n) {
  const m = new Map(), s = new Set(), a = [];
  let t = 0;
  for (let i = 0; i < n; i++) {
    m.set(i & 15, i); s.add(i & 7); a.push(i);
    if (m.has(i & 3)) t += m.get(i & 3);
    if (s.has(i & 1)) t += 1;
    if (i % 5 === 0) { m.delete(i & 15); s.delete(i & 7); t += a.pop(); }
  }
  m.clear();
  return t + m.size + s.size + a.length;
}
let ct = 0;
for (let r = 0; r < 200; r++) ct += colls(50);
out.push(ct);
// User methods with the intrinsics' names: the helpers decline.
class Queue { constructor() { this.items = []; } push(x) { this.items.push(x * 2); return 7; } pop() { return this.items.length; } get(k) { return k + 1; } has(k) { return k > 2; } }
function useQueue(q, n) { let t = 0; for (let i = 0; i < n; i++) { t += q.push(i); if (q.has(i)) t += q.get(i); } return t + q.pop(); }
let qt = 0;
for (let r = 0; r < 200; r++) qt += useQueue(new Queue(), 20);
out.push(qt);
// Sorts with string and mixed comparators.
function cmp(a, b) { return a < b ? -1 : a > b ? 1 : 0; }
function sortAll(list) { const c = list.slice(); c.sort(cmp); return c.join(","); }
let so = "";
for (let r = 0; r < 120; r++) so = sortAll(words) + sortAll([5, 3, 9, 1, 7]);
out.push(so, sortAll(["b", "a", 3, 1]), sortAll([NaN, 2, 1]));
// A class declared per call: its value changes, the guard sees it.
function makeKind(k) {
  class Kind { constructor(v) { this.v = v + k; } twice() { return new Kind(this.v); } }
  return Kind;
}
const K1 = makeKind(1);
function useKind(n) { let x = new K1(0); for (let i = 0; i < n; i++) x = x.twice(); return x.v; }
let kt = 0;
for (let r = 0; r < 200; r++) kt += useKind(10);
const K2 = makeKind(100);
out.push(kt, useKind(3), new K2(1).twice().v, new K2(1).twice() instanceof K2, new K1(1).twice() instanceof K1);
// >>> by constants on Ints and doubles, and ToInt32 of boxed doubles.
function bits(xs) { let h = 0; for (let i = 0; i < xs.length; i++) { const x = xs[i]; h = (h ^ (x >>> 3) ^ (x >>> 31)) + ((x | 0) >>> 1); } return h; }
const xs = [1, -1, 2.75, -3.5, 4294967295, 2147483648, 1e10, -1e10, 0.5, NaN];
let bt = 0;
for (let r = 0; r < 300; r++) bt = bits(xs);
out.push(bt, bits([Infinity, -0]));
console.log(out.join(" "));
"####;

#[test]
fn matches_node() {
    // Read once per process, before the first VM: this file's only test.
    std::env::set_var("ZIPP_OPT", "1");
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
