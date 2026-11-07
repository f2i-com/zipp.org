//! Typed-array element reads and writes through Tier C's receiver cache, against Node: every kind, views, detach, transfer, resize, slot reuse.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"Int8Array 44 0,0,-1,-1,-1,0,-1,7
Uint8Array 1068 0,0,255,255,255,0,255,7
Uint8ClampedArray 2554 0,0,255,0,255,255,0,255
Int16Array -45268 0,0,255,-1,-1,0,-1,-31225
Uint16Array 479020 0,0,255,65535,65535,0,65535,34311
Int32Array 1230196524 0,0,255,-1,-1,-2147483648,2147483647,-1589934585
Uint32Array 31294967596 0,0,255,4294967295,4294967295,2147483648,2147483647,2705032711
Float32Array NaN 0,NaN,255,-1,4294967296,2147483648,-2147483648,7000000000
Float64Array NaN 0,NaN,255,-1,4294967295,2147483648,-2147483649,7000000007.5
oob 40,,,,30,,
views 1145060408 145 19 undefined
alternate 12
pair 5.5
transfer 5015 0
transferToFixedLength 5012.5 0
fixed before 40 track 80
fixed shrunk 8000 track 4020 0 4
fixed regrown 48 track 288 48
resize mid-loop 32048
churn 56000
alias NaN
write Int8Array 0,0,1,-1,127,-128,-1,0,127,-1,0,-1,0,-1,0,1,2,0,0,0,0,0,0,0,0,1,7,0,0
write Uint8Array 0,0,1,255,127,128,255,0,127,255,0,255,0,255,0,1,2,0,0,0,0,0,0,0,0,1,7,0,0
write Uint8ClampedArray 0,0,1,0,127,128,255,255,0,255,255,255,0,255,0,2,2,0,255,0,0,255,0,255,0,1,7,0,0
write Int16Array 0,0,1,-1,127,128,255,256,-129,-1,0,-1,0,-1,0,1,2,0,-7168,7168,0,0,0,0,0,1,7,0,0
write Uint16Array 0,0,1,65535,127,128,255,256,65407,65535,0,65535,0,65535,0,1,2,0,58368,7168,0,0,0,0,0,1,7,0,0
write Int32Array 0,0,1,-1,127,128,255,256,-129,65535,65536,2147483647,-2147483648,-1,0,1,2,0,1410065408,-1410065408,0,0,0,0,0,1,7,0,0
write Uint32Array 0,0,1,4294967295,127,128,255,256,4294967167,65535,65536,2147483647,2147483648,4294967295,0,1,2,0,1410065408,2884901888,0,0,0,0,0,1,7,0,0
write Float32Array 0,0,1,-1,127,128,255,256,-129,65535,65536,2147483648,-2147483648,4294967296,0.5,1.5,2.5,-0.5,10000000000,-10000000000,NaN,Infinity,-Infinity,3.3999999521443642e+38,0,1,7,0,NaN
write Float64Array 0,0,1,-1,127,128,255,256,-129,65535,65536,2147483647,-2147483648,4294967295,0.5,1.5,2.5,-0.5,10000000000,-10000000000,NaN,Infinity,-Infinity,3.4e+38,1e-46,1,7,0,NaN
valueOf 42
f32 bits 192
write after transfer 0 0"####;

const PROGRAM: &str = r####"// Typed-array element reads in functions compiled whole (Tier C), where the
// receiver's buffer moves, detaches or shrinks between reads, every kind.
function hot(f, n = 3000) { let r; for (let i = 0; i < n; i++) r = f(i); return r; }
const out = [];
const log = (...a) => out.push(a.map(v => Object.is(v, -0) ? "-0" : String(v)).join(" "));

function sum(ta, n) { let s = 0; for (let i = 0; i < n; i++) s += ta[i]; return s; }
function at(ta, i) { return ta[i]; }
function two(a, b, i) { return a[i] + b[i]; }

// Every kind, with values at the edges of its range.
const kinds = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array, Float32Array, Float64Array];
for (const K of kinds) {
  const ta = new K(16);
  for (let i = 0; i < 16; i++) ta[i] = (i - 8) * 1e9 + i * 0.5;
  ta[0] = -0; ta[1] = NaN; ta[2] = 255; ta[3] = -1; ta[4] = 4294967295; ta[5] = 2147483648; ta[6] = -2147483649;
  log(K.name, hot(() => sum(ta, 16), 500), [0, 1, 2, 3, 4, 5, 6, 15].map(i => at(ta, i)).join());
}

// Out of range, negative, fractional and string keys.
const u8 = Uint8Array.from([10, 20, 30, 40]);
log("oob", hot(() => [at(u8, 3), at(u8, 4), at(u8, -1), at(u8, 1.5), at(u8, "2"), at(u8, "x"), at(u8, 2 ** 32)].join()));

// A view with an offset into a larger buffer, and a subarray.
const big = new Uint8Array(64).map((_, i) => i);
const view = new Uint32Array(big.buffer, 8, 4);
const sub = big.subarray(10, 20);
log("views", hot(() => sum(view, 4)), hot(() => sum(sub, 10)), at(sub, 9), at(sub, 10));

// The same register through two arrays in turn, and a dense array.
const a1 = new Int32Array([1, 2, 3]), a2 = new Float64Array([0.5, 1.5, 2.5]), plain = [7, 8, 9];
log("alternate", hot(i => at(i & 1 ? a1 : a2, i % 3) + at(plain, i % 3)));
log("pair", hot(i => two(a1, a2, i % 3)));

// Detach by transfer while a compiled loop has the array cached.
function drain(ta, n, act) { let s = 0; for (let i = 0; i < n; i++) { if (i === 5) act(); s += ta[i] === undefined ? 1000 : ta[i]; } return s; }
for (let r = 0; r < 300; r++) drain(new Uint8Array(10).fill(1), 10, () => {});
{
  const ta = new Uint8Array(10).fill(3);
  log("transfer", drain(ta, 10, () => ta.buffer.transfer()), ta.length);
}
{
  const ta = new Float64Array(10).fill(2.5);
  log("transferToFixedLength", drain(ta, 10, () => ta.buffer.transferToFixedLength(4)), ta.length);
}

// A resizable buffer shrinks and grows under a fixed-length and a length-tracking view.
{
  const rab = new ArrayBuffer(16, { maxByteLength: 64 });
  const fixed = new Uint8Array(rab, 0, 8).fill(4);
  const track = new Uint8Array(rab).fill(5);
  log("fixed before", hot(() => sum(fixed, 8)), "track", hot(() => sum(track, 16)));
  rab.resize(4);
  log("fixed shrunk", drain(fixed, 8, () => {}), "track", drain(track, 8, () => {}), fixed.length, track.length);
  rab.resize(48);
  new Uint8Array(rab).fill(6);
  log("fixed regrown", drain(fixed, 8, () => {}), "track", drain(track, 48, () => {}), track.length);
  log("resize mid-loop", drain(track, 40, () => rab.resize(8)));
}

// Many arrays made and dropped while one function keeps reading: slots get reused.
let churn = 0;
for (let r = 0; r < 2000; r++) {
  const t = new Int16Array(8).fill(r & 7);
  churn += sum(t, 8);
  if (r % 100 === 0) { const junk = []; for (let k = 0; k < 2000; k++) junk.push({ k }); }
}
log("churn", churn);

// Element writes and reads interleave, through another view of the same buffer.
{
  const f64 = new Float64Array(4), bytes = new Uint8Array(f64.buffer);
  function poke(i) { bytes[i] = 0xff; return at(f64, 0); }
  log("alias", hot(i => poke(i % 8)));
}
// Element writes of every kind of value into every kind, in a compiled loop.
function fill(ta, vals) { for (let i = 0; i < ta.length; i++) ta[i] = vals[i % vals.length]; return ta; }
const vals = [0, -0, 1, -1, 127, 128, 255, 256, -129, 65535, 65536, 2147483647, -2147483648, 4294967295,
  0.5, 1.5, 2.5, -0.5, 1e10, -1e10, NaN, Infinity, -Infinity, 3.4e38, 1e-46, true, "7", null, undefined];
for (let r = 0; r < 200; r++) fill(new Int32Array(32), [r, r + 0.5]);
for (const K of kinds) {
  const ta = fill(new K(vals.length), vals);
  log("write", K.name, Array.from(ta).join());
}
const obj = { valueOf() { return 42; } };
log("valueOf", hot(() => fill(new Uint16Array(3), [obj, 1, 2])[0], 300));
{
  const f = new Float32Array(4), back = new Uint8Array(f.buffer);
  function wr(i, v) { f[i] = v; return back[i * 4 + 3]; }
  log("f32 bits", hot(i => wr(i & 3, (i & 1) ? -2.5 : 1 / 3), 3000));
}
// A write after the buffer was transferred away is dropped.
{
  const ta = new Uint8Array(8);
  function wloop(n, act) { for (let i = 0; i < n; i++) { if (i === 4) act(); ta[i] = 9; } return ta.length; }
  for (let r = 0; r < 300; r++) wloop(8, () => {});
  const before = ta.buffer;
  log("write after transfer", wloop(8, () => before.transfer()), new Uint8Array(before.byteLength).length);
}
console.log(out.join("\n"));
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
