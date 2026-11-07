//! Element reads of every typed-array kind in compiled code: in and out of bounds,
//! views, subarrays, resize and detach. Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"Int8Array -7500 -97 undefined undefined undefined -26 8 undefined
Uint8Array 376500 159 undefined undefined undefined 230 8 undefined
Uint8ClampedArray 159375 159 undefined undefined undefined 0 8 undefined
Int16Array 88500 159 undefined undefined undefined -26 8 undefined
Uint16Array 73816500 159 undefined undefined undefined 65510 8 undefined
Int32Array 88500 159 undefined undefined undefined -26 8 undefined
Uint32Array 4831838296500 159 undefined undefined undefined 4294967270 8 undefined
Float32Array 90000 159.5 undefined undefined undefined -25.5 8 undefined
Float64Array 90000 159.5 undefined undefined undefined -25.5 8 undefined
-7500 -5n undefined
7500 5n
5153960754600 true 2147483647 2147483648 4294967295 -1 2147483649
true true Infinity 1.5 2 true
true true 0.10000000149011612
3852000 770 1798 undefined
16500 4 7 undefined
3375 9 9
undefined 4 undefined 0
3 16 0 8
6000 8
undefined 0 8
undefined yes 0
0
5 b 7 1"####;

const PROGRAM: &str = r####"// Element reads of every typed-array kind in hot compiled code, in and out of bounds.
const HOT = 3000;
function rd(t, i) { return t[i]; }
function hot(t) { var s = 0; for (var i = 0; i < HOT; i++) { var v = rd(t, i % t.length); s += typeof v === "bigint" ? Number(v) : v; } return s; }
var mk = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array, Float32Array, Float64Array];
for (var K of mk) {
  var t = new K(8);
  for (var i = 0; i < 8; i++) t[i] = i * 37 - 100 + (K === Float32Array || K === Float64Array ? 0.5 : 0);
  console.log(K.name, hot(t), rd(t, 7), rd(t, 8), rd(t, -1), rd(t, 1.5), rd(t, "2"), rd(t, "length"), rd(t, 4294967296));
}
var big = new BigInt64Array(2); big[1] = -5n; console.log(hot(big), rd(big, 1), rd(big, 2));
var ubig = new BigUint64Array(2); ubig[1] = 5n; console.log(hot(ubig), rd(ubig, 1));
var u32 = new Uint32Array([0, 1, 0x7fffffff, 0x80000000, 0xffffffff]);
console.log(hot(u32), Object.is(rd(u32, 0), 0), rd(u32, 2), rd(u32, 3), rd(u32, 4), rd(u32, 4) | 0, rd(u32, 3) + 1);
var f = new Float64Array([NaN, -0, Infinity, 1.5, 2]);
console.log(Object.is(rd(f, 0), NaN), Object.is(rd(f, 1), -0), rd(f, 2), rd(f, 3), rd(f, 4), Number.isInteger(rd(f, 4)));
var f32 = new Float32Array([NaN, -0, 0.1]);
console.log(Object.is(rd(f32, 0), NaN), Object.is(rd(f32, 1), -0), rd(f32, 2));
// A view with an offset, a subarray, a length-tracking view, resize, detach.
var ab = new ArrayBuffer(16); new Uint8Array(ab).forEach((_, i, a) => { a[i] = i; });
var v = new Uint16Array(ab, 2, 3); console.log(hot(v), rd(v, 0), rd(v, 2), rd(v, 3));
var s = new Uint8Array(ab).subarray(4, 8); console.log(hot(s), rd(s, 0), rd(s, 3), rd(s, 4));
var rb = new ArrayBuffer(8, { maxByteLength: 16 }), tr = new Uint8Array(rb), fx = new Uint8Array(rb, 0, 8);
tr[7] = 9; console.log(hot(tr), rd(tr, 7), rd(fx, 7));
rb.resize(4); console.log(rd(tr, 7), tr.length, rd(fx, 0), fx.length);
rb.resize(16); tr[15] = 3; console.log(rd(tr, 15), tr.length, rd(fx, 0), fx.length);
var db = new ArrayBuffer(4), dv = new Uint8Array(db); dv[1] = 8; console.log(hot(dv), rd(dv, 1));
var moved = db.transfer(); console.log(rd(dv, 1), dv.length, new Uint8Array(moved)[1]);
// Prototypes do not answer an integer index; a named key goes on past the element read.
Object.prototype[20] = "no"; Uint8Array.prototype.q = "yes";
var u = new Uint8Array(2); console.log(rd(u, 20), rd(u, "q"), rd(u, 1));
delete Object.prototype[20]; delete Uint8Array.prototype.q;
var pu = new Proxy(new Uint8Array(2), {}); try { console.log(rd(pu, 0)); } catch (e) { console.log(e.name); }
console.log(rd(new Proxy([5], {}), 0), rd("ab", 1), rd([7], 0), rd({ 0: 1 }, 0));
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
