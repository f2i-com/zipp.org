// json/small-many: many small JSON.parse / JSON.stringify round trips of
// varied API-message-shaped objects (a few keys, a nested user object, a short
// array of line items, nullable fields), plus a reviver, a function replacer,
// an array (whitelist) replacer, the indent argument, nested arrays and
// string escaping (quotes, backslashes, control characters, non-ASCII, emoji
// surrogate pairs, lone surrogates).
// Why a likely weak spot: per-call setup dominates for small inputs (the
// parser's entry, property-key interning, object creation per level, the
// stringifier's per-value type dispatch and the escape scan), and reviver /
// replacer calls are interpreter-to-script callbacks per key.
// Dominant operations: JSON.stringify of ~150-300 byte objects, JSON.parse of
// the same, reviver/replacer callbacks, escape handling of short strings.
const SIZE = 20000;

let seed = 40503;
function rnd() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return (seed >>> 0) / 4294967296; }
function rint(n) { return Math.floor(rnd() * n); }
function fnv(h, s) {
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
}

const NAMES = ["alice", "bob", "carol", "dave", "Ève", "Zoë", "李雷", "韩梅梅", "Ørjan", "José"];
const WORDS = ["ok", "say \"hi\"", "back\\slash", "tab\there", "line\nbreak", "ctl\u0001\u001f",
  "café", "naïve", "日本語", "emoji 😀🎉", "plain text value", "a/b/c", "\u2028sep\u2029",
  "lone \ud800 high", "x"];
const TYPES = ["create", "update", "delete", "ping", "batch"];

function makeMessage(i) {
  const n = 1 + rint(4);
  const items = [];
  for (let k = 0; k < n; k++) {
    items.push({ sku: "SKU-" + (1000 + rint(9000)), qty: 1 + rint(20), price: rint(100000) / 100 });
  }
  const m = {
    id: i,
    type: TYPES[i % TYPES.length],
    ok: (i & 1) === 0,
    score: rint(2000001) / 1000 - 1000,
    user: { name: NAMES[rint(NAMES.length)], age: 18 + rint(60), email: "u" + rint(100000) + "@example.org" },
    tags: [WORDS[rint(WORDS.length)], WORDS[rint(WORDS.length)]],
    items: items,
    meta: (i % 3 === 0) ? null : { trace: "t-" + i.toString(16), depth: rint(5), _secret: "hidden" + i },
  };
  if (i % 7 === 0) m.nested = [[1, [2, [3, [4]]]], [], [[], [[]]], [true, false, null]];
  if (i % 11 === 0) m.big = 1e21 + rint(1000) * 1e15;
  if (i % 13 === 0) m.negz = -0;
  return m;
}

function reviver(key, value) {
  if (typeof value === "number" && key === "qty") return value * 2;
  if (key === "_secret") return undefined;
  return value;
}
function replacer(key, value) {
  if (key.length > 0 && key.charCodeAt(0) === 95) return undefined; // drop "_..." keys
  if (typeof value === "number" && key === "price") return Math.round(value);
  return value;
}
const WHITELIST = ["id", "type", "user", "name", "items", "sku", "qty"];

let hashStr = 2166136261 >>> 0;
let totalLen = 0;
let roundTrips = 0;
let qtySum = 0;
let reviverHash = 0;
let replacerLen = 0;
let indentLen = 0;
let parsedFields = 0;

for (let i = 0; i < SIZE; i++) {
  const m = makeMessage(i);
  const s = JSON.stringify(m);
  totalLen += s.length;
  if ((i & 7) === 0) hashStr = fnv(hashStr, s);
  else hashStr = Math.imul(hashStr ^ s.length, 16777619) >>> 0;
  const back = JSON.parse(s);
  if (JSON.stringify(back) === s) roundTrips++;
  parsedFields += Object.keys(back).length + back.items.length + back.user.name.length;
  qtySum = (qtySum + back.items[0].qty + (back.tags[0] === m.tags[0] ? 1 : 0)) | 0;

  switch (i % 4) {
    case 0: {
      const r = JSON.parse(s, reviver);
      reviverHash = (reviverHash + r.items[r.items.length - 1].qty + (r.meta && r.meta._secret === undefined ? 3 : 1)) | 0;
      break;
    }
    case 1: {
      const t = JSON.stringify(m, replacer);
      replacerLen += t.length;
      break;
    }
    case 2: {
      const t = JSON.stringify(m, WHITELIST);
      replacerLen += t.length * 3;
      break;
    }
    default: {
      const t = JSON.stringify(m.items, null, (i & 8) ? 2 : "\t");
      indentLen += t.length;
      if ((i & 255) === 3) hashStr = fnv(hashStr, t);
      break;
    }
  }
}

// A handful of hand-written inputs: whitespace, escapes, exponent forms.
const FIXED = [
  ' { "a" : [ 1 , 2.5e3 , -0.125 , 1E-7 ] , "b" : "\\u00e9\\ud83d\\ude00\\n\\"q\\"" , "c" : { } } ',
  '[[[[[[[[[[0]]]]]]]]]]',
  '{"k\\u0000ey":"\\/\\b\\f\\r\\t","n":null,"t":true,"f":false}',
  '"\\ud800"',
];
let fixedHash = 2166136261 >>> 0;
for (let r = 0; r < Math.max(1, SIZE >> 4); r++) {
  for (let k = 0; k < FIXED.length; k++) {
    const v = JSON.parse(FIXED[k]);
    fixedHash = fnv(fixedHash, JSON.stringify(v));
  }
}

console.log("messages " + SIZE + " roundtrips " + roundTrips + " totalLen " + totalLen);
console.log("hash " + hashStr + " fields " + parsedFields + " qty " + qtySum);
console.log("reviver " + reviverHash + " replacer " + replacerLen + " indent " + indentLen);
console.log("fixed " + fixedHash);
