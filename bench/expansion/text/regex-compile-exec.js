// regex/compile-vs-exec: the cost of compiling many distinct regular
// expressions (new RegExp over generated pattern strings, each made unique so
// no source cache can serve it, plus a small set of repeated patterns that a
// cache could serve) against the cost of executing one compiled regex many
// times: exec with numbered and named captures, test, a global regex driven
// by a lastIndex loop, String.prototype.match/matchAll, and replace with a
// function callback and with $1/$<name> substitution strings.
// Why a likely weak spot: the regex compiler's fixed cost per pattern (parse,
// program build, flag handling) and the per-exec overhead around the matcher
// (match-object and capture-array allocation, groups object creation,
// lastIndex reads/writes, the callback call per replacement) rather than the
// matching itself, which is short here.
// Dominant operations: RegExp constructor, RegExp.prototype.exec/test,
// String.prototype.replace with a function, matchAll iteration.
const SIZE = 40000;

let seed = 40503;
function rnd() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return (seed >>> 0) / 4294967296; }
function rint(n) { return Math.floor(rnd() * n); }
function fnv(h, s) {
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
}
function mix(h, n) { return Math.imul(h ^ (n | 0), 16777619) >>> 0; }

const USERS = ["alice", "bob", "carol", "dave", "erin", "frank", "grace", "heidi"];
const ACTIONS = ["get", "put", "post", "delete", "patch"];
function pad2(n) { return n < 10 ? "0" + n : "" + n; }
function makeLine() {
  return "20" + pad2(10 + rint(20)) + "-" + pad2(1 + rint(12)) + "-" + pad2(1 + rint(28)) +
    "T" + pad2(rint(24)) + ":" + pad2(rint(60)) +
    " user=" + USERS[rint(USERS.length)] + " id=" + rint(100000) +
    " action=" + ACTIONS[rint(ACTIONS.length)] + " status=" + (200 + rint(4) * 100 + rint(5)) +
    " path=/api/v" + (1 + rint(3)) + "/items/" + rint(5000) + " took=" + rint(2000) + "ms";
}
const POOL = [];
for (let i = 0; i < 1024; i++) POOL.push(makeLine());

// ---- A. compile many distinct patterns -------------------------------------
const PIECES = ["[a-z]+", "\\d{2,4}", "(?:get|put|post)", "\\s*", "[A-Z][a-z]*", "(\\w)\\1",
  "x?y+", "[^,;]{1,8}", "\\b", "(\\d+)", "[0-9a-f]{2}", "(?<w>[a-z]{3})", "\\.", "=\\w+", "(?:ab|cd)*"];
const FLAGS = ["", "i", "g", "m", "s", "gi", "y", "u"];
const SAMPLES = ["say get 1234 now", "Hello World aa", "xyyy, 0f0e;", "user=alice id=42", "abcdab.x", "ZZ 99 == q"];
let compileHash = 2166136261 >>> 0;
let compileMatches = 0;
const NCOMPILE = Math.max(4, SIZE >> 2);
for (let i = 0; i < NCOMPILE; i++) {
  const n = 2 + rint(4);
  let body = "";
  let named = false;
  for (let k = 0; k < n; k++) {
    let p = PIECES[rint(PIECES.length)];
    if (p.indexOf("?<w>") >= 0) { if (named) p = "([a-z]{3})"; named = true; }
    body += p;
  }
  const src = body + "|#" + i.toString(36);
  const re = new RegExp(src, FLAGS[i & 7]);
  const str = SAMPLES[i % SAMPLES.length];
  const m = re.exec(str);
  if (m !== null) { compileMatches++; compileHash = mix(compileHash, m.index * 31 + m[0].length + m.length); }
  if (re.test(str)) compileMatches++;
  compileHash = mix(compileHash, re.source.length + re.lastIndex * 7 + re.flags.length * 1000);
}
// The same 16 sources compiled again and again (a source cache can serve these).
let repeatHash = 0;
const REPEAT = [];
for (let k = 0; k < 16; k++) REPEAT.push("(\\w+)=(\\d+)|q" + k + "[a-z]{2,5}");
for (let i = 0; i < NCOMPILE; i++) {
  const re = new RegExp(REPEAT[i & 15], (i & 16) ? "g" : "");
  repeatHash = mix(repeatHash, re.test(POOL[i & 1023]) ? i & 255 : 1);
}

// ---- B. one regex, many exec calls with numbered and named captures --------
const LINE_RE = /^(?<year>\d{4})-(?<month>\d\d)-(?<day>\d\d)T(\d\d):(\d\d) user=(\w+) id=(\d+) action=(\w+) status=(\d{3})/;
let execSum = 0;
let execHash = 2166136261 >>> 0;
for (let i = 0; i < SIZE; i++) {
  const m = LINE_RE.exec(POOL[i & 1023]);
  const g = m.groups;
  execSum = (execSum + (+g.year) * 3 + (+g.month) + (+g.day) + (+m[4]) + (+m[5]) + (+m[7]) + (+m[9])) | 0;
  execHash = mix(execHash, m[6].length * 16 + m[8].length);
}
let testCount = 0;
const TEST_RE = /status=(?:4\d\d|5\d\d)/;
const TEST_RE2 = /path=\/api\/v2\/items\/\d*7 /;
for (let i = 0; i < SIZE; i++) {
  if (TEST_RE.test(POOL[(i * 7) & 1023])) testCount++;
  if (TEST_RE2.test(POOL[(i * 13) & 1023])) testCount += 2;
}

// ---- C. a global regex driven by lastIndex over a long record --------------
let record = "";
for (let i = 0; i < 200; i++) record += "k" + i.toString(36) + "=" + rint(1000000) + (i % 5 === 4 ? "; " : ", ");
const KV_RE = /(\w+)=(\d+)/g;
let kvCount = 0, kvSum = 0, kvHash = 0;
const NREC = Math.max(1, (SIZE / 40) | 0);
for (let r = 0; r < NREC; r++) {
  KV_RE.lastIndex = (r * 3) % 17;
  let m;
  while ((m = KV_RE.exec(record)) !== null) {
    kvCount++;
    kvSum = (kvSum + (+m[2]) + m.index) | 0;
    kvHash = mix(kvHash, m[1].length + KV_RE.lastIndex);
  }
}

// ---- D. match / matchAll / replace -----------------------------------------
let matchCount = 0, matchHash = 2166136261 >>> 0;
const WORD_G = /[a-z]+/g;
const NUM_NAMED = /(?<key>[a-z]+)=(?<val>\d+)/g;
for (let i = 0; i < SIZE >> 1; i++) {
  const line = POOL[(i * 5) & 1023];
  const words = line.match(WORD_G);
  matchCount += words.length;
  for (const mm of line.matchAll(NUM_NAMED)) {
    matchHash = mix(matchHash, mm.groups.key.length * 1000 + mm.groups.val.length + mm.index);
  }
}
let replHash = 2166136261 >>> 0;
let callbacks = 0;
function onPair(m, k, v, off) { callbacks++; return v + ":" + k.length + "@" + off; }
for (let i = 0; i < SIZE >> 1; i++) {
  const line = POOL[(i * 3) & 1023];
  const a = line.replace(/(\w+)=(\w+)/g, onPair);
  const b = line.replace(LINE_RE, "$<day>/$<month>/$<year> $6#$7");
  const c = line.replace(/\b([a-z])([a-z]*)\b/g, function (m, first, rest) { return first.toUpperCase() + rest; });
  const d = line.replace(/(\d+)ms$/, "$1 milliseconds ($&)");
  replHash = mix(replHash, a.length * 7 + b.length * 5 + c.length * 3 + d.length);
  if ((i & 63) === 0) { replHash = fnv(replHash, a); replHash = fnv(replHash, b); replHash = fnv(replHash, c); replHash = fnv(replHash, d); }
}

console.log("compile " + NCOMPILE + " matches " + compileMatches + " hash " + compileHash + " repeat " + repeatHash);
console.log("exec " + SIZE + " sum " + execSum + " hash " + execHash + " test " + testCount);
console.log("lastIndex " + kvCount + " sum " + kvSum + " hash " + kvHash);
console.log("match " + matchCount + " hash " + matchHash + " replace " + replHash + " callbacks " + callbacks);
