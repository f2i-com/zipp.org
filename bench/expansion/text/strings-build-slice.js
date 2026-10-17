// strings/build-slice: everyday string manipulation on one-byte (ASCII)
// strings: building a large string by += (append, rope-like) and by
// prepending, the same content by array push + join, then slice/substring/
// indexOf/lastIndexOf/includes/startsWith/endsWith at many offsets of the
// large string, split/join, replace/replaceAll with string patterns,
// padStart/padEnd/trim/repeat/at/case mapping on short strings, and repeated
// charCodeAt / [] / charAt reads at pseudo-random indices of the large string.
// Why a likely weak spot: += building either copies (quadratic) or makes a
// rope that must be flattened on first indexed read; slice/substring may copy
// or share; each short-string op allocates a result, so allocation and the
// built-in call path dominate; random-index reads into a rope or into a
// sliced string are slow if every read walks a tree or re-flattens.
// Dominant operations: string concatenation, flattening, String.prototype
// slice/indexOf/split/replaceAll/padStart, charCodeAt at random indices.
const SIZE = 150000;

let seed = 40503;
function rnd() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return (seed >>> 0) / 4294967296; }
function rint(n) { return Math.floor(rnd() * n); }
function fnv(h, s) {
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
}
function mix(h, n) { return Math.imul(h ^ (n | 0), 16777619) >>> 0; }

// ---- A. building: += append, prepend, array join ---------------------------
let big = "";
for (let i = 0; i < SIZE; i++) {
  big += "rec" + i + ":" + ((i * 7) % 1000) + ";";
  if ((i & 15) === 15) big += "\n";
}
const parts = [];
for (let i = 0; i < SIZE; i++) {
  parts.push("rec" + i + ":" + ((i * 7) % 1000) + ";");
  if ((i & 15) === 15) parts.push("\n");
}
const joined = parts.join("");
const sameBuild = joined === big ? 1 : 0;
let pre = "";
for (let i = 0; i < (SIZE >> 4); i++) pre = (i % 10) + pre;
let nested = "";
for (let i = 0; i < (SIZE >> 4); i++) {
  let line = "";
  for (let k = 0; k < 6; k++) line += `${k}-${i & 255}|`;
  nested += line + "\n";
}
const buildHash = mix(mix(mix(big.length, joined.length), pre.length * 7 + pre.charCodeAt(pre.length >> 1)), nested.length);

// ---- B. slice/substring/indexOf/lastIndexOf/includes/startsWith ------------
const L = big.length;
let scanSum = 0, scanHash = 2166136261 >>> 0, hits = 0;
for (let i = 0; i < SIZE; i++) {
  const off = rint(L - 128);
  const w = big.slice(off, off + 64);
  const a = w.indexOf(";");
  const b = w.lastIndexOf(":");
  const c = big.indexOf("rec", off);
  const d = big.lastIndexOf(";", off + 40);
  const sub = big.substring(off + 10, off);
  scanSum = (scanSum + a + b + (c - off) + (off - d) + sub.length) | 0;
  if (w.includes("rec1")) hits++;
  if (w.startsWith("rec")) hits += 2;
  if (big.startsWith("rec", c)) hits += 4;
  if (w.endsWith(";")) hits += 8;
  if ((i & 31) === 0) scanHash = fnv(scanHash, w + sub);
}
let missing = 0;
for (let r = 0; r < 8; r++) { // a fixed number of full scans keeps the run linear in SIZE
  if (big.indexOf("rec-" + r) < 0) missing++;
  if (!big.includes("::" + r)) missing++;
  missing += big.lastIndexOf("rec0:") === 0 ? 1 : 0;
}

// ---- C. split/join, replace/replaceAll, pad/trim/repeat/case ---------------
let cHash = 2166136261 >>> 0, fields = 0;
const lines = big.split("\n");
for (let i = 0; i < lines.length; i++) {
  const f = lines[i].split(";");
  fields += f.length;
  const j = f.join("|");
  const r1 = j.replace("rec", "REC");
  const r2 = r1.replaceAll(":", "=");
  const r3 = r2.replaceAll("|rec", "/");
  cHash = mix(cHash, j.length * 3 + r1.length + r2.charCodeAt(r2.length >> 1) + r3.length * 5);
}
let shortHash = 2166136261 >>> 0;
for (let i = 0; i < SIZE; i++) {
  const n = String((i * 2654435761) >>> 0);
  const p = n.padStart(12, "0") + "|" + n.padEnd(11, ".");
  const t = ("  \t" + n + " \n").trim();
  const u = t.trimStart().trimEnd().repeat(1 + (i & 3));
  const v = (i & 1) ? u.toUpperCase() : ("Ab" + u + "Cd").toLowerCase();
  const e = p.at(-1) + p.at(3) + v.at(0);
  shortHash = mix(shortHash, p.length + t.length * 13 + u.length * 7 + v.length + e.charCodeAt(1));
  if ((i & 127) === 0) shortHash = fnv(shortHash, p + v + e);
}

// ---- D. random-index reads into the large string ----------------------------
let rSum = 0, semis = 0, dHash = 2166136261 >>> 0;
for (let i = 0; i < SIZE * 6; i++) {
  const j = (rnd() * L) | 0;
  rSum = (rSum + big.charCodeAt(j)) | 0;
  if (big[j] === ";") semis++;
  if ((i & 7) === 0) dHash = mix(dHash, joined.charAt(L - 1 - j).charCodeAt(0));
}
const sl = big.slice(L >> 2, (L >> 2) + (L >> 1));
for (let i = 0; i < SIZE; i++) {
  const j = (i * 7919) % sl.length;
  rSum = (rSum + sl.charCodeAt(j) * 3) | 0;
}

console.log("build " + big.length + " same " + sameBuild + " hash " + buildHash);
console.log("scan " + scanSum + " hits " + hits + " missing " + missing + " hash " + scanHash);
console.log("split " + lines.length + " fields " + fields + " hash " + cHash + " short " + shortHash);
console.log("random " + rSum + " semis " + semis + " hash " + dHash);
