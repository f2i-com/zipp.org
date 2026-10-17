// numbers/format-parse: number <-> string conversion and numeric built-ins:
// Number.prototype.toString(radix) on integers, toFixed / toPrecision /
// toExponential across their digit ranges, String(double) for awkward values
// (arbitrary bit patterns via Float64Array, subnormals, values near powers of
// ten, 2^53 neighbours) with a round-trip check, parseInt / parseFloat /
// Number() on generated numeric strings (radix prefixes, exponents,
// whitespace, trailing junk, signed zero, overflow), Math functions (sqrt,
// pow, exp, log, sin, atan2, hypot, cbrt, imul, clz32, fround), and BigInt
// arithmetic on 64-256-bit values (add, sub, mul, div, mod, shifts,
// asUintN/asIntN, comparison) with BigInt toString / BigInt(string).
// Why a likely weak spot: shortest round-trip double printing and exact
// decimal formatting (toFixed/toPrecision need exact big-decimal arithmetic
// for large exponents), string-to-double parsing, and arbitrary-precision
// BigInt allocation per operation are all library code whose per-call cost
// matters far more than the interpreter loop around it.
// Dominant operations: Number#toString, toFixed/toPrecision/toExponential,
// String(double), parseFloat/Number(), BigInt mul/div/toString.
// Implementation-approximated Math results (pow, exp, log, sin, atan2, hypot,
// cbrt) are quantised before hashing; exact ones are hashed in full.
const SIZE = 40000;

let seed = 40503;
function rnd() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return (seed >>> 0) / 4294967296; }
function rint(n) { return Math.floor(rnd() * n); }
function u32() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; const a = seed >>> 16; seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return ((a << 16) | (seed >>> 16)) >>> 0; }
function fnv(h, s) {
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
}
function mix(h, n) { return Math.imul(h ^ (n | 0), 16777619) >>> 0; }

const f64 = new Float64Array(1);
const u32v = new Uint32Array(f64.buffer);
function randomDouble() {
  // A finite double from random bits (exponent kept away from NaN/Infinity).
  for (;;) {
    u32v[0] = u32();
    u32v[1] = u32();
    const x = f64[0];
    if (x === x && x !== Infinity && x !== -Infinity) return x;
  }
}
function niceDouble() {
  switch (rint(6)) {
    case 0: return rint(1000000000) / 1000;
    case 1: return (rint(2000000) - 1000000) * 1.1;
    case 2: return rint(100000) * 1e-9;
    case 3: return rint(1000) * 1e18 + rint(1000);
    case 4: return 1 / (1 + rint(1000));
    default: return (rint(100000) + 0.5) / 128;
  }
}

// ---- A. toString(radix) / parseInt(radix) on integers ----------------------
const RADIX = [2, 8, 16, 36, 7, 3, 10, 32];
let radixHash = 2166136261 >>> 0, radixOk = 0;
for (let i = 0; i < SIZE; i++) {
  const n = (i & 1) ? (u32() * 2097152 + (u32() >>> 11)) : -(u32() >>> (i & 31));  // up to 2^53, or a negative int
  const r = RADIX[i & 7];
  const s = n.toString(r);
  if (parseInt(s, r) === n) radixOk++;
  radixHash = mix(radixHash, s.length * 37 + s.charCodeAt(s.length - 1));
  if ((i & 15) === 0) radixHash = fnv(radixHash, s);
}

// ---- B. toFixed / toPrecision / toExponential --------------------------------
let fmtHash = 2166136261 >>> 0, fmtLen = 0;
for (let i = 0; i < SIZE; i++) {
  const x = (i & 3) === 3 ? randomDouble() : niceDouble();
  const a = Math.abs(x) < 1e21 ? x.toFixed(i % 11) : x.toFixed(2);
  const b = x.toPrecision(1 + (i % 21));
  const c = x.toExponential(i % 17);
  fmtLen += a.length + b.length * 3 + c.length * 7;
  fmtHash = fnv(fmtHash, b);
  if ((i & 3) === 0) { fmtHash = fnv(fmtHash, a); fmtHash = fnv(fmtHash, c); }
}
const FIXED_CASES = [[1.005, 2], [1.45, 1], [0.5, 0], [1.5, 0], [2.5, 0], [-1.5, 0], [1e21, 2], [123.456, 10],
  [0.000001, 7], [1.255, 2], [8.345, 2], [-0, 2], [5e-324, 3], [1.7976931348623157e308, 0], [0.1, 20]];
let fixedHash = 2166136261 >>> 0;
for (let k = 0; k < FIXED_CASES.length; k++) {
  const x = FIXED_CASES[k][0], d = FIXED_CASES[k][1];
  fixedHash = fnv(fixedHash, x.toFixed(d) + "|" + x.toPrecision(1 + d % 20) + "|" + x.toExponential(d % 20));
}

// ---- C. String(double) and the round trip ----------------------------------
const AWKWARD = [0.1 + 0.2, 1 / 3, 5e-324, 2.2250738585072014e-308, 2.225073858507201e-308, 1.7976931348623157e308,
  9007199254740993, 9007199254740992, 9007199254740991, 1e21, 1e-7, 1e-6, 123456789012345680000, -1e-7, 4.35,
  0.000001, 1e23, 8.41e21, 5e-7, 2 ** -1074 * 3, 0.3, 100, 1e300 * 10, -0, 2 ** 63, 2 ** 64 + 2 ** 12];
let strHash = 2166136261 >>> 0, strLen = 0, roundTrip = 0;
for (let i = 0; i < SIZE * 2; i++) {
  const x = (i & 1) ? randomDouble() : (i & 2) ? niceDouble() : AWKWARD[(i >> 2) % AWKWARD.length];
  const s = String(x);
  strLen += s.length;
  if (Number(s) === x || (x !== x)) roundTrip++;
  strHash = fnv(strHash, s);
}

// ---- D. parseInt / parseFloat / Number() on generated strings ---------------
const PREFIX = ["", " ", "\t\n ", "+", "-", "  -", "0", "00"];
const SUFFIX = ["", "px", " ", "e", "e5", "e-3", "E+2", ".", ".5", "abc", "_1", " "];
function numText(i) {
  let digits = "";
  const nd = 1 + rint(17);
  for (let k = 0; k < nd; k++) digits += String.fromCharCode(48 + rint(10));
  if ((i & 3) === 1) { const p = rint(digits.length + 1); digits = digits.slice(0, p) + "." + digits.slice(p); }
  return PREFIX[rint(PREFIX.length)] + digits + SUFFIX[rint(SUFFIX.length)];
}
const SPECIAL_TEXT = ["0x1f", "0X7FFFFFFF", "0b101", "0o17", "-0", "  -0  ", "Infinity", "-Infinity", "1e400", "-1e400",
  "1e-400", ".5", "5.", "", " ", "0x", "1_000", "١٢٣", "  42  ", "+.5e+1", "0.0000001", "1e21", "12345678901234567890"];
let parseHash = 2166136261 >>> 0, nanCount = 0, negZero = 0;
function addParsed(v) {
  if (v !== v) { nanCount++; return; }
  if (v === 0 && 1 / v < 0) negZero++;
  if (v === Infinity || v === -Infinity) { parseHash = mix(parseHash, v > 0 ? 77 : 78); return; }
  parseHash = fnv(parseHash, String(v));
}
for (let i = 0; i < SIZE; i++) {
  const t = (i & 15) === 15 ? SPECIAL_TEXT[(i >> 4) % SPECIAL_TEXT.length] : numText(i);
  addParsed(parseInt(t));
  addParsed(parseFloat(t));
  addParsed(Number(t));
  if ((i & 7) === 0) addParsed(parseInt(t, 16 + (i & 16)));
}

// ---- E. Math functions --------------------------------------------------------
let exactHash = 2166136261 >>> 0, approxHash = 2166136261 >>> 0;
for (let i = 0; i < SIZE * 4; i++) {
  const a = rnd() * 1000 + 1e-3;
  const b = rnd() * 20 - 10;
  const n = u32() | 0;
  exactHash = mix(exactHash, Math.imul(n, 0x9e3779b1) ^ Math.clz32(n >>> (i & 31)));
  f64[0] = Math.sqrt(a);
  exactHash = mix(mix(exactHash, u32v[0]), u32v[1]);
  f64[0] = Math.fround(a * b);
  exactHash = mix(exactHash, u32v[1] ^ u32v[0]);
  const q = Math.floor(Math.pow(a, b / 8) * 64) + Math.floor(Math.exp(b) * 64) + Math.floor(Math.log(a) * 4096) +
    Math.floor(Math.sin(a) * 4096) + Math.floor(Math.atan2(b, a - 500) * 4096) + Math.floor(Math.hypot(a, b, i & 7) * 64) +
    Math.floor(Math.cbrt(a * b) * 4096) + Math.floor(Math.cos(b) * 4096) + Math.floor(Math.tanh(b / 4) * 4096);
  approxHash = mix(approxHash, q);
}

// ---- F. BigInt arithmetic and toString ----------------------------------------
function randBig(words) {
  let x = 0n;
  for (let k = 0; k < words; k++) x = (x << 32n) | BigInt(u32());
  return x;
}
let bigHash = 2166136261 >>> 0, bigLen = 0, bigCmp = 0;
const MASK32 = 0xffffffffn;
const NB = Math.max(4, SIZE >> 1);
for (let i = 0; i < NB; i++) {
  const words = 2 + (i & 6);                   // 64..256 bits
  const a = randBig(words);
  const b = randBig(1 + (i % words)) | 1n;
  const neg = (i & 1) ? -a : a;
  const s = a + b, d = a - b * 3n, p = a * b, q = neg / b, r = neg % b;
  const sh = (a << BigInt(i & 63)) >> BigInt((i >> 3) & 31);
  const u = BigInt.asUintN(64, p) ^ BigInt.asIntN(64, d);
  bigHash = mix(bigHash, Number(s & MASK32));
  bigHash = mix(bigHash, Number(p & MASK32) ^ Number((p >> 64n) & MASK32));
  bigHash = mix(bigHash, Number(q & MASK32));
  bigHash = mix(bigHash, Number(r & MASK32));
  bigHash = mix(bigHash, Number(sh & MASK32) ^ Number(u & 0xffffn));
  if (p > s) bigCmp++;
  if (q < 0n) bigCmp += 2;
  if (i % 3 === 0) {
    const t = p.toString();
    const h = q.toString(16);
    bigLen += t.length + h.length;
    if (BigInt(t) === p) bigCmp += 4;
    if ((i & 31) === 0) { bigHash = fnv(bigHash, t); bigHash = fnv(bigHash, h); bigHash = fnv(bigHash, String(r)); }
  }
}

console.log("radix " + radixOk + " hash " + radixHash);
console.log("format " + fmtLen + " hash " + fmtHash + " fixed " + fixedHash);
console.log("string " + strLen + " roundtrip " + roundTrip + " hash " + strHash);
console.log("parse " + parseHash + " nan " + nanCount + " negzero " + negZero);
console.log("math exact " + exactHash + " approx " + approxHash);
console.log("bigint " + bigHash + " len " + bigLen + " cmp " + bigCmp);
