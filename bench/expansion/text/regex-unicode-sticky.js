// regex/unicode-sticky: regular expressions with the u flag over non-ASCII
// text (Latin accents, Greek, Cyrillic, CJK, Hangul, emoji outside the BMP
// as surrogate pairs), \p{L}/\p{N}/\p{Lu}/\p{Script=...} property escapes,
// astral-range classes, case-insensitive u matching, lookbehind (positive and
// negative), split by regex (including empty-width lookaround splits), and a
// sticky (y) tokenizer for a small synthetic source language: once as one
// combined alternation of capture groups, once as a list of sticky regexes
// tried in order at lastIndex (the hand-written lexer shape).
// Why a likely weak spot: u-mode matching has to step by code point and
// decode surrogate pairs, property escapes become large range tables, and a
// sticky tokenizer performs one short anchored exec per token, so the fixed
// per-exec overhead (lastIndex read/write, match array, captures) dominates.
// Dominant operations: exec/test with /u and /y, String.prototype.match with
// /gu, split by regex, lookbehind, property-escape class membership.
const SIZE = 12000;

let seed = 40503;
function rnd() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return (seed >>> 0) / 4294967296; }
function rint(n) { return Math.floor(rnd() * n); }
function mix(h, n) { return Math.imul(h ^ (n | 0), 16777619) >>> 0; }

const WORDS = ["café", "naïve", "über", "Ångström", "señor", "œuvre", "façade", "Ærø", "straße",
  "Ελλάδα", "Москва", "日本語", "中文", "漢字", "한국어", "emoji😀", "🎉", "👍", "🚀ship",
  "x²", "½", "٣٤", "Ⅻ", "42", "3.14", "$19.99", "€5", "£12.50", "camelCaseWord", "HTTPServer",
  "ſtar", "Zoë", "Dvořák", "Łódź", "İstanbul", "the", "and", "of"];
const SEPS = [" ", ", ", "; ", " | ", "  "];
const POOL = [];
for (let i = 0; i < 256; i++) {
  const n = 8 + rint(7);
  let s = WORDS[rint(WORDS.length)];
  for (let k = 1; k < n; k++) s += SEPS[rint(SEPS.length)] + WORDS[rint(WORDS.length)];
  POOL.push(s);
}

// ---- A. property escapes, code points, lookbehind, split ---------------------
const LETTERS = /\p{L}+/gu;
const NUMBERS = /\p{N}+/gu;
const CODEPOINTS = /./gsu;
const CODEUNITS = /./gs;
const PRICE = /(?<=[$€£])\d+(?:\.\d+)?/g;
const CAPWORD = /(?<!\p{L})\p{Lu}\p{Ll}+/gu;
const NOT_AFTER_DIGIT = /(?<![\d.])\d+(?![\d.])/g;
const ASTRAL = /[\u{1F300}-\u{1F6FF}]/gu;
const HAN = /\p{Script=Han}+/gu;
const GREEK_CYR = /[\p{Script=Greek}\p{Script=Cyrillic}]+/gu;
const SPLIT_SEP = /\s*[,;|]\s*|\s+/u;
const CAMEL = /(?<=\p{Ll})(?=\p{Lu})/u;
let letterCount = 0, letterLen = 0, numCount = 0, cpCount = 0, cuCount = 0;
let priceSum = 0, capCount = 0, astralCount = 0, hanLen = 0, gcLen = 0, splitCount = 0, camelParts = 0, dotNums = 0;
const NTEXT = Math.max(1, SIZE >> 1);
for (let i = 0; i < NTEXT; i++) {
  const s = POOL[i & 255];
  const w = s.match(LETTERS);
  if (w !== null) { letterCount += w.length; for (let k = 0; k < w.length; k++) letterLen += w[k].length; }
  const n = s.match(NUMBERS);
  if (n !== null) numCount += n.length;
  cpCount += s.match(CODEPOINTS).length;
  cuCount += s.match(CODEUNITS).length;
  const p = s.match(PRICE);
  if (p !== null) for (let k = 0; k < p.length; k++) priceSum += Math.round(+p[k] * 100);
  const c = s.match(CAPWORD);
  if (c !== null) capCount += c.length;
  const a = s.match(ASTRAL);
  if (a !== null) astralCount += a.length;
  const h = s.match(HAN);
  if (h !== null) for (let k = 0; k < h.length; k++) hanLen += h[k].length;
  const g = s.match(GREEK_CYR);
  if (g !== null) for (let k = 0; k < g.length; k++) gcLen += g[k].length;
  const d = s.match(NOT_AFTER_DIGIT);
  if (d !== null) dotNums += d.length;
  const parts = s.split(SPLIT_SEP);
  splitCount += parts.length;
  if ((i & 3) === 0) {
    for (let k = 0; k < parts.length; k++) camelParts += parts[k].split(CAMEL).length;
  }
}

// ---- B. case-insensitive unicode matching (fixed probes, spec-exact) ---------
const PROBES = [
  [/ÅNGSTRÖM/iu, "ångström"], [/Å/iu, "å"], [/Å/i, "å"], [/ſ/iu, "s"], [/ſ/i, "s"],
  [/straße/iu, "STRASSE"], [/ΣΊΣΥΦΟΣ/iu, "σίσυφος"], [/σ/iu, "ς"], [/МОСКВА/iu, "москва"],
  [/[\p{Lu}]/u, "é"], [/[^\p{Ll}]/u, "é"], [/^\p{L}+$/u, "日本語"], [/^.$/u, "😀"], [/^.$/, "😀"],
  [/^[😀]$/u, "😀"], [/\u{1F600}/u, "😀"], [/\uD83D/u, "😀"], [/\uD83D/, "😀"],
  [/\bcafé\b/u, "a café b"], [/é\b/u, "café"],
];
let probeBits = 0;
for (let r = 0; r < Math.max(1, SIZE >> 3); r++) {
  probeBits = 0;
  for (let k = 0; k < PROBES.length; k++) if (PROBES[k][0].test(PROBES[k][1])) probeBits |= 1 << k;
}

// ---- C. sticky tokenizer over a synthetic source language -------------------
const IDS = ["größe", "名前", "zähler", "flag", "x", "valeur", "ñandú", "count", "λ", "имя"];
const OPS = ["+", "-", "*", "/", "==", "!=", "<=", ">=", "&&", "||", "<", ">"];
const STRS = ["\"héllo\"", "\"w\\\"örld\\\" 😀\"", "\"日本\\n\"", "\"\"", "\"tab\\tend\""];
let program = "";
const NSTMT = Math.max(4, SIZE);
for (let i = 0; i < NSTMT; i++) {
  const a = IDS[rint(IDS.length)], b = IDS[rint(IDS.length)];
  switch (i % 4) {
    case 0: program += "let " + a + " = " + rint(1000) + " " + OPS[rint(OPS.length)] + " " + b + " * " + (rint(100) / 4) + ";\n"; break;
    case 1: program += "if (" + a + " " + OPS[rint(OPS.length)] + " " + rint(50) + " && " + b + ") { emit(" + STRS[rint(STRS.length)] + ", " + a + "); }\n"; break;
    case 2: program += "// 注释 comment " + i + " ünïcödé\n"; break;
    default: program += a + " = f(" + b + ", " + STRS[rint(STRS.length)] + ", " + rint(9) + "." + rint(99) + ");\n"; break;
  }
}
const TOK = /(\s+)|(\/\/[^\n]*)|([\p{L}_$][\p{L}\p{N}_$]*)|(\d+(?:\.\d+)?)|("(?:[^"\\\n]|\\.)*")|(==|!=|<=|>=|&&|\|\||[-+*\/=<>!(){};,.])/uy;
const kinds = [0, 0, 0, 0, 0, 0, 0];
let tokHash = 2166136261 >>> 0;
let pos = 0;
TOK.lastIndex = 0;
while (pos < program.length) {
  const m = TOK.exec(program);
  if (m === null) { kinds[0]++; pos++; TOK.lastIndex = pos; continue; }
  let k = 1;
  while (m[k] === undefined) k++;
  kinds[k]++;
  if (k !== 1) tokHash = mix(tokHash, k * 64 + m[0].length);
  pos = TOK.lastIndex;
}
const LEX = [/\s+/uy, /\/\/[^\n]*/uy, /[\p{L}_$][\p{L}\p{N}_$]*/uy, /\d+(?:\.\d+)?/uy, /"(?:[^"\\\n]|\\.)*"/uy,
  /==|!=|<=|>=|&&|\|\||[-+*\/=<>!(){};,.]/uy];
const kinds2 = [0, 0, 0, 0, 0, 0, 0];
let tokHash2 = 2166136261 >>> 0;
pos = 0;
while (pos < program.length) {
  let k = 0;
  for (; k < LEX.length; k++) {
    const re = LEX[k];
    re.lastIndex = pos;
    if (re.test(program)) break;
  }
  if (k === LEX.length) { kinds2[0]++; pos++; continue; }
  const end = LEX[k].lastIndex;
  kinds2[k + 1]++;
  if (k !== 0) tokHash2 = mix(tokHash2, (k + 1) * 64 + (end - pos));
  pos = end;
}

console.log("letters " + letterCount + " len " + letterLen + " numbers " + numCount + " cp " + cpCount + " cu " + cuCount);
console.log("price " + priceSum + " cap " + capCount + " astral " + astralCount + " han " + hanLen + " gc " + gcLen + " dot " + dotNums);
console.log("split " + splitCount + " camel " + camelParts + " probes " + probeBits);
console.log("program " + program.length + " tokens " + kinds.join(",") + " hash " + tokHash);
console.log("lexer " + kinds2.join(",") + " hash " + tokHash2);
