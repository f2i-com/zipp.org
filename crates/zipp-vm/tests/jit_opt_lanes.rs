//! Tier O over Tier C's lanes: typed arrays through the record lane (a
//! double into an integer kind declines the stub), class getters inlined and
//! redefined, constructors through the allocation lane, calls through the
//! cross-call lanes, against Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"6432509 NaN NaN 0abc 0 NaN 0.5 -16.5 2660shape 11shape 2350 -20shape 387400 6765 49"####;

const PROGRAM: &str = r####"// Tier O: typed arrays through the record lane, class getters inlined,
// constructors through the allocation lane, calls through the cross lanes.
const out = [];
function sumTA(a, n) { let s = 0; for (let i = 0; i < n; i++) s += a[i]; return s; }
function fillTA(a, n, k) { for (let i = 0; i < n; i++) a[i] = (i * k) % 300 - 20.5; return a[n - 1]; }
const u8 = new Uint8Array(64), i32 = new Int32Array(64), f64 = new Float64Array(64), c8 = new Uint8ClampedArray(64);
let acc = 0;
for (let r = 0; r < 300; r++) {
  for (const a of [u8, i32, f64, c8]) acc += fillTA(a, 64, r & 7) + sumTA(a, 64);
}
out.push(acc);
// Past the end, a plain Array, a string, holes, a key that is no index.
const plain = [1, 2, 3, , 5];
out.push(sumTA(u8, 70), sumTA(plain, 5), sumTA("abc", 3), sumTA(f64, 0));
const holey = []; holey[3] = 4;
out.push(String(sumTA(holey, 4)), fillTA(new Float32Array(8), 8, 3), fillTA([0, 0, 0], 3, 2));
class Shape {
  constructor(w, h) { this.w = w; this.h = h; }
  get area() { return this.w * this.h; }
  get kind() { return "shape"; }
  scaled(k) { return new Shape(this.w * k, this.h * k); }
}
class Square extends Shape {
  constructor(s) { super(s, s); }
  get kind() { return "square"; }
}
function areas(list) { let t = 0; let ks = ""; for (const s of list) { t += s.area; ks = s.kind; } return t + ks; }
function grow(s, n) { let x = s; for (let i = 0; i < n; i++) x = x.scaled(1.01); return Math.round(x.area); }
const shapes = [];
for (let k = 0; k < 20; k++) shapes.push(new Shape(k, k + 1));
let as = "";
for (let r = 0; r < 200; r++) as = areas(shapes);
out.push(as, areas([new Square(3), new Shape(1, 2)]), grow(new Shape(2, 3), 300));
// A getter redefined under compiled code.
Object.defineProperty(Shape.prototype, "area", { get() { return -1; } });
out.push(areas(shapes));
// Calls the lanes make: recursion, a method of an own property, a class method.
function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
const calc = { k: 3, mul(x) { return x * this.k; } };
function useCalc(n) { let t = 0; for (let i = 0; i < n; i++) t += calc.mul(i) + fib(i & 7); return t; }
let uc = 0;
for (let r = 0; r < 100; r++) uc += useCalc(50);
out.push(uc, fib(20));
calc.mul = function (x) { return x - this.k; };
out.push(useCalc(10));
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
