// strings/unicode-scan: non-ASCII (two-byte) string handling: codePointAt
// walks and String.fromCodePoint rebuilds over text with surrogate pairs,
// for-of / spread / Array.from code-point iteration, toUpperCase /
// toLowerCase on Latin-1, Latin Extended, Greek (including the context-
// sensitive final sigma), Cyrillic and the length-changing special cases
// (sharp s, dotted capital I, ligatures), normalize('NFC'/'NFD'/'NFKC'/
// 'NFKD') on composed and decomposed text (accents, Hangul syllables,
// compatibility characters), indexOf/lastIndexOf/includes with non-ASCII and
// ASCII needles in a large two-byte string, and encodeURIComponent /
// decodeURIComponent / encodeURI / decodeURI round trips.
// Why a likely weak spot: engines optimise one-byte strings; two-byte and
// surrogate-pair paths, the string iterator protocol, Unicode case tables
// with special casing, and normalization (decompose, reorder, recompose)
// are slower generic code that interpreters rarely tune.
// Dominant operations: codePointAt, fromCodePoint, the string iterator,
// toUpperCase/toLowerCase, normalize, indexOf on two-byte strings, URI coding.
const SIZE = 20000;

let seed = 40503;
function rnd() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return (seed >>> 0) / 4294967296; }
function rint(n) { return Math.floor(rnd() * n); }
function fnv(h, s) {
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
}
function mix(h, n) { return Math.imul(h ^ (n | 0), 16777619) >>> 0; }

// Code point ranges with long-stable assignments.
const RANGES = [[0x61, 0x7a], [0x41, 0x5a], [0x30, 0x39], [0xe0, 0xff], [0x100, 0x17f], [0x3b1, 0x3c9],
  [0x430, 0x44f], [0x4e00, 0x4fff], [0xac00, 0xad00], [0x1f600, 0x1f64f], [0x1f300, 0x1f3ff], [0x20, 0x20]];
function randCp() {
  const r = RANGES[rint(RANGES.length)];
  return r[0] + rint(r[1] - r[0] + 1);
}

// ---- A. codePointAt walk and String.fromCodePoint rebuild ------------------
const cps = [];
for (let i = 0; i < SIZE * 4; i++) cps.push(randCp());
// The 16-code-point chunks are copied into a scratch array rather than taken
// with cps.slice(i, i + 16) (see the case notes: that slice is O(cps.length)
// per call on one engine, which would make this an array benchmark).
let text = "";
const chunk = [];
for (let i = 0; i < cps.length; i += 16) {
  const n = Math.min(16, cps.length - i);
  chunk.length = n;
  for (let k = 0; k < n; k++) chunk[k] = cps[i + k];
  text += String.fromCodePoint.apply(null, chunk);
}
let cpSum = 0, cpCount = 0, astral = 0;
for (let i = 0; i < text.length;) {
  const c = text.codePointAt(i);
  cpSum = (cpSum + c) | 0;
  cpCount++;
  if (c > 0xffff) { astral++; i += 2; } else i++;
}
let rebuilt = "";
for (let i = 0; i < cps.length; i++) rebuilt += String.fromCodePoint(cps[i]);
const rebuildSame = rebuilt === text ? 1 : 0;
let lowSurr = 0;
for (let i = 0; i < text.length; i += 3) {
  const c = text.codePointAt(i);
  if (c >= 0xdc00 && c <= 0xdfff) lowSurr++;
}

// ---- B. code-point iteration (for-of, spread, Array.from) -------------------
let iterCount = 0, iterHash = 2166136261 >>> 0;
for (const ch of text) {
  iterCount++;
  iterHash = mix(iterHash, ch.length * 0x10000 + ch.charCodeAt(ch.length - 1));
}
let spreadLen = 0;
for (let i = 0; i + 64 <= text.length; i += 64) {
  const chunk = text.slice(i, i + 64);
  spreadLen += [...chunk].length + Array.from(chunk).length;
}

// ---- C. case mapping on non-ASCII ------------------------------------------
const CASE_WORDS = ["straße", "İstanbul", "ΟΔΥΣΣΕΥΣ", "ΣΊΣΥΦΟΣ ΚΑΙ", "Ελλάδα", "МОСКВА", "Дмитрий",
  "œuvre", "Ærø", "Ångström", "ﬁnale", "ﬂow", "Ǆemal", "ǅ", "ŉ", "Dvořák", "Łódź", "çà et là", "ÿ",
  "µ", "ǰ", "ΐ", "ﬀ", "ŀl", "Ĳssel", "groß ß ẞ"];
const caseSrc = [];
for (let i = 0; i < 512; i++) {
  let s = CASE_WORDS[rint(CASE_WORDS.length)];
  for (let k = 0; k < 3; k++) s += " " + CASE_WORDS[rint(CASE_WORDS.length)];
  caseSrc.push(s);
}
let caseHash = 2166136261 >>> 0, caseLen = 0;
for (let i = 0; i < SIZE; i++) {
  const s = caseSrc[i & 511];
  const u = s.toUpperCase();
  const l = s.toLowerCase();
  const ul = u.toLowerCase();
  caseLen += u.length * 3 + l.length * 2 + ul.length;
  if ((i & 15) === 0) { caseHash = fnv(caseHash, u); caseHash = fnv(caseHash, l); caseHash = fnv(caseHash, ul); }
}

// ---- D. normalize -----------------------------------------------------------
const NORM_WORDS = ["café", "café", "Å", "Å", "Å", "한국어", "한",
  "ﬁ", "①", "ｶ", "Ǆ", "ệ", "ệ", "ȫ", "ñ", "ñ", "x̖́",
  "plain", "ﬃ", "²", "Ω", "Ω", "가나다", "ṩ"];
const normSrc = [];
for (let i = 0; i < 512; i++) {
  let s = "";
  for (let k = 0; k < 5; k++) s += NORM_WORDS[rint(NORM_WORDS.length)];
  normSrc.push(s);
}
let normHash = 2166136261 >>> 0, normLen = 0, normEq = 0;
for (let i = 0; i < SIZE; i++) {
  const s = normSrc[i & 511];
  const c = s.normalize("NFC");
  const d = s.normalize("NFD");
  normLen += c.length * 7 + d.length;
  if (d.normalize("NFC") === c) normEq++;
  if ((i & 3) === 0) {
    const kc = s.normalize("NFKC");
    const kd = s.normalize("NFKD");
    normLen += kc.length * 3 + kd.length * 5;
    if ((i & 63) === 0) { normHash = fnv(normHash, kc); normHash = fnv(normHash, kd); }
  }
  if ((i & 31) === 0) { normHash = fnv(normHash, c); normHash = fnv(normHash, d); }
}

// ---- E. indexOf scanning in a large two-byte string ---------------------------
// The pieces are cut on code-point boundaries, so `big` stays well formed (no
// lone surrogates; see the case notes for why that matters).
const NEEDLES = ["日本", "😀", "ß", "Привет", "abc", "é", "一丁", "z0"];
function isTrail(u) { return u >= 0xdc00 && u <= 0xdfff; }
let big = "";
for (let i = 0; i < SIZE; i++) {
  let a = (i * 37) % (text.length - 32);
  if (isTrail(text.charCodeAt(a))) a++;
  let b = a + 12;
  if (isTrail(text.charCodeAt(b))) b++;
  big += text.slice(a, b);
  if (i % 97 === 0) big += NEEDLES[i % NEEDLES.length];
}
const BL = big.length;
let idxSum = 0, idxHits = 0;
// Find every occurrence, forwards and backwards (the common scanning loop).
for (let k = 0; k < NEEDLES.length; k++) {
  const needle = NEEDLES[k];
  let pos = big.indexOf(needle);
  while (pos >= 0) { idxHits++; idxSum = (idxSum + pos) | 0; pos = big.indexOf(needle, pos + 1); }
  pos = big.lastIndexOf(needle);
  while (pos > 0) { idxHits++; idxSum = (idxSum ^ pos) | 0; pos = big.lastIndexOf(needle, pos - 1); }
  if (big.includes(needle, BL >> 1)) idxHits += 3;
}
// Searches from pseudo-random start positions in a document of bounded size.
const doc = big.slice(0, Math.min(BL, 16000));
const DL = doc.length;
for (let i = 0; i < SIZE; i++) {
  const needle = NEEDLES[i & 7];
  const from = (i * 7919) % DL;
  const a = doc.indexOf(needle, from);
  if (a >= 0) { idxHits++; idxSum = (idxSum + a - from) | 0; }
  if ((i & 1) === 0) {
    const b = doc.lastIndexOf(needle, from);
    if (b >= 0) { idxHits++; idxSum = (idxSum + from - b) | 0; }
  }
}
for (let i = 0; i < 8; i++) { // a fixed number of full scans keeps the run linear in SIZE
  if (big.includes("Ānever" + i)) idxHits += 1000;
  idxSum = (idxSum + big.indexOf("ab" + (i & 7))) | 0;
}

// ---- F. URI encoding round trips ---------------------------------------------
const URI_WORDS = ["héllo wörld", "a+b=c&d", "日本語/テキスト", "emoji 😀🎉", "100% sure", "#frag?x=1",
  "naïve café", "~!*'()", "Ελλάδα", "plain-text_value.ok", "@:$,;", "Ω≈ç√"];
let uriHash = 2166136261 >>> 0, uriLen = 0, uriOk = 0;
for (let i = 0; i < SIZE; i++) {
  const s = URI_WORDS[i % URI_WORDS.length] + " " + URI_WORDS[(i * 7) % URI_WORDS.length] + i;
  const e = encodeURIComponent(s);
  const d = decodeURIComponent(e);
  if (d === s) uriOk++;
  const e2 = encodeURI(s);
  const d2 = decodeURI(e2.replace("%25", "%2523"));
  uriLen += e.length * 3 + e2.length + d2.length;
  if ((i & 63) === 0) { uriHash = fnv(uriHash, e); uriHash = fnv(uriHash, e2); uriHash = fnv(uriHash, d2); }
}

console.log("cp " + cpCount + " sum " + cpSum + " astral " + astral + " len " + text.length + " same " + rebuildSame + " low " + lowSurr);
console.log("iter " + iterCount + " hash " + iterHash + " spread " + spreadLen);
console.log("case " + caseLen + " hash " + caseHash);
console.log("norm " + normLen + " eq " + normEq + " hash " + normHash);
console.log("index " + idxSum + " hits " + idxHits + " big " + BL);
console.log("uri " + uriLen + " ok " + uriOk + " hash " + uriHash);
