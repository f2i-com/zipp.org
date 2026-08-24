"use strict";
// phase-timed copy of bench/real/typedarray-math.js -- TOP LEVEL, single shot,
// identical statement order/shapes to the original. Only Date.now() marks added.
var T = [];
var _t = Date.now();
function mark(n) { var t = Date.now(); T.push(n + "=" + (t - _t)); _t = t; }

let NF = 8000000;
let NI = 8000000;

let x = new Float64Array(NF), y = new Float64Array(NF);
mark("alloc_f64");
for (let i = 0; i < NF; i++) {
  x[i] = (((i * 2654435761) >>> 0) % 100000) / 100000;
  y[i] = (((i * 40503 + 12345) >>> 0) % 100000) / 100000;
}
mark("f64_fill");
let a = 1.25;
for (let i = 0; i < NF; i++) y[i] = a * x[i] + y[i]; // axpy
mark("axpy");
let dot = 0;
for (let i = 0; i < NF; i++) dot += x[i] * y[i];
mark("dot");
let inv = 1 / Math.sqrt(dot);
let nsum = 0;
for (let i = 0; i < NF; i++) { x[i] *= inv; nsum += x[i]; }
mark("normalize");

let iv = new Int32Array(NI);
mark("alloc_i32");
let st = 0x9E3779B9 | 0;
for (let i = 0; i < NI; i++) {
  st ^= st << 13; st ^= st >>> 17; st ^= st << 5;
  iv[i] = st | 0;
}
mark("i32_xorshift");
for (let i = 1; i < NI; i++) iv[i] = (iv[i] + iv[i - 1]) | 0;
mark("i32_prefix");
let ilast = iv[NI - 1];
let imid = iv[NI >> 1];

let dv = new DataView(iv.buffer, 0, 4096 * 4);
let bsum = 0;
for (let r = 0; r < 6000; r++) {
  for (let o = 0; o < 4096 * 4; o += 4) {
    var le = (o >> 2) & 1;
    var v = dv.getUint32(o, le === 1);
    bsum = (bsum + (v >>> 24) + (v & 255) + dv.getUint16(o, le === 0) + dv.getInt8(o + 2)) | 0;
  }
}
mark("dataview");
let u8 = new Uint8Array(iv.buffer, 0, 64);
let u8sum = 0;
for (let i = 0; i < 64; i++) u8sum = (u8sum + u8[i] * (i + 1)) | 0;
mark("u8");

console.log("dot=" + dot.toFixed(4) + " nsum=" + nsum.toFixed(6) + " ilast=" + ilast +
  " imid=" + imid + " bsum=" + bsum + " u8sum=" + u8sum);
console.log("PHASES " + T.join(" "));
