//! Tier O's breadth (property writes, Math, %, new, class values, captured
//! variables, globals, string constants, generic reads, deopt feedback)
//! against Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"1051618.054803 NaN -3.25 -0.3076923076923077 NaN 8.058950 57t1 3f 9t1 2 4 t0 13500 a 20834250 1400 68851.65154134115 7200 99.944834 105 ReferenceError no len"####;

const PROGRAM: &str = r####"// Tier O breadth: Math, %, new, class values, property writes, globals,
// captured variables, string constants, generic property reads.
var G = 0;
var GS = "g";
const out = [];
function mathy(x, y) {
  return Math.sqrt(x * x + y * y) + Math.abs(x - y) + Math.floor(x / 3) + Math.ceil(y / 7) +
    Math.trunc(-x / 5) + Math.round(x / 4) + Math.min(x, y) + Math.max(x, -y) + Math.imul(x | 0, 31) +
    Math.pow(y, 2) + Math.atan2(y, x + 1) + Math.hypot(x, y) + Math.sin(x) + Math.exp(-y);
}
function mods(a, b) { return (a % b) + (a % 7) + ((-a) % 3) + (a % 0.75); }
class Vec {
  constructor(x, y) { this.x = x; this.y = y; }
  add(o) { return new Vec(this.x + o.x, this.y + o.y); }
  scale(k) { return new Vec(this.x * k, this.y * k); }
  get len() { return Math.sqrt(this.x * this.x + this.y * this.y); }
  set both(v) { this.x = v; this.y = v; }
}
function vecs(n) {
  let v = new Vec(0, 0);
  for (let i = 0; i < n; i++) v = v.add(new Vec(i % 3, i % 5)).scale(0.5);
  return v.x + v.y + v.len;
}
function writes(o, n) {
  for (let i = 0; i < n; i++) { o.x = i; o.y = o.x * 2; o.tag = "t" + (i & 1); }
  return o.x + o.y + o.tag;
}
function globalsW(n) { for (let i = 0; i < n; i++) { G = G + i; GS = i & 1 ? "a" : "b"; } return G; }
function counter() {
  let c = 0, s = "s";
  return function step(k) { c += k; return c + s.length + (s === "s" ? 1 : 0); };
}
function strs(o) { return o.kind === "bin" ? 1 : o.kind === "un" ? 2 : 3; }
function reads(o) { return o.len + o.x + "abc".length + (o.missing === undefined ? 1 : 0); }
function poly(o) { return o.a + o.b; }
let acc = 0;
for (let k = 0; k < 400; k++) acc += mathy(k * 0.37, k % 11 - 5);
out.push(acc.toFixed(6));
acc = 0;
for (let k = 1; k < 400; k++) acc += mods(k * 3 - 600, (k % 9) - 4);
out.push(acc, mods(-4, 2), 1 / mods(-4, 2), mods(5, 0));
out.push(vecs(300).toFixed(6));
const w1 = { x: 0, y: 0, tag: "" };
let ws = "";
for (let k = 0; k < 200; k++) ws = writes(w1, 20);
out.push(ws);
// A setter, a frozen object (sloppy: the write is ignored), another shape.
const vv = new Vec(1, 2);
for (let k = 0; k < 200; k++) writes(k & 1 ? vv : w1, 3);
const fz = Object.freeze({ x: 1, y: 2, tag: "f" });
out.push(writes(fz, 2), writes({ tag: 1, y: 0, x: 0 }, 4), vv.x, vv.y, vv.tag);
for (let k = 0; k < 300; k++) globalsW(10);
out.push(G, GS);
const st = counter();
let cs = 0;
for (let k = 0; k < 500; k++) cs += st(k);
out.push(cs);
let sc = 0;
const kinds = [{ kind: "bin" }, { kind: "un" }, { kind: "x" + "y" }, { kind: "b" + "in" }];
for (let k = 0; k < 800; k++) sc += strs(kinds[k & 3]);
out.push(sc);
let rs = 0;
for (let k = 0; k < 300; k++) rs += reads(k & 1 ? new Vec(k, 1) : { len: 2, x: k });
out.push(rs);
let ps = 0;
const shapes = [{ a: 1, b: 2 }, { b: 3, a: 4 }, { z: 0, a: 5, b: 6 }, { get a() { return 7; }, b: 8 }];
for (let k = 0; k < 800; k++) ps += poly(shapes[k & 3]);
out.push(ps);
// Math replaced under compiled code, then the global `Math` itself.
Math.sqrt = function (x) { return -x; };
out.push(mathy(3, 4).toFixed(6));
const realMath = Math;
globalThis.Math = { sqrt: () => 1, abs: () => 2, floor: () => 3, ceil: () => 4, trunc: () => 5, round: () => 6,
  min: () => 7, max: () => 8, imul: () => 9, pow: () => 10, atan2: () => 11, hypot: () => 12, sin: () => 13, exp: () => 14 };
out.push(mathy(3, 4));
globalThis.Math = realMath;
// A throw from a captured variable's TDZ, a getter that throws.
function tdz() { const f = () => late; let r = 0; try { r = f(); } catch (e) { r = e.constructor.name; } let late = 5; return r; }
out.push(tdz());
const bad = { get len() { throw new Error("no len"); }, x: 1 };
try { reads(bad); } catch (e) { out.push(e.message); }
console.log(out.join(" "));
"####;

#[test]
fn matches_node() {
    // Read once per process, before the first VM: this file's only test.
    std::env::set_var("ZIPP_OPT", "1");
    std::env::set_var("ZIPP_NO_OPT_TIER_UP", "1");
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
