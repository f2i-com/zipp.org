"use strict";
// real-world bench 1: build ~2MB synthetic JS-like source, tokenize it with a
// hand-written charCodeAt scanner, then run a recursive-descent expression
// parser over the `var <id> = <expr>;` token spans. Deterministic (mulberry32).
//
// WHAT THIS ROW MEASURES — read the name carefully. It is USERLAND SOURCE
// TOKENIZATION, a parser written IN JavaScript. It does NOT benchmark zipp's own
// parser or compiler: the engine parses this ~200-line file once and then runs it.
// So the row is a string / array / branch / call workload that happens to be
// shaped like a parser, and it belongs in the same mental bucket as
// `markdown-render`, not in any claim about frontend speed.
//
// A real frontend comparison (parse+compile a fixed source WITHOUT executing it,
// against node's parse/compile time, with source generation outside the timed
// phase) is a separate benchmark this suite does not yet have.
function mulberry32(seed) {
  var a = seed | 0;
  return function () {
    a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
var rnd = mulberry32(0xC0FFEE);
function ri(n) { return (rnd() * n) | 0; }

// ---- deterministic source generation ----
var LETTERS = "abcdefghijklmnopqrstuvwxyz";
var POOL = [];
for (var pi = 0; pi < 600; pi++) {
  POOL.push(LETTERS[ri(26)] + LETTERS[ri(26)] + LETTERS[ri(26)] + "_" + pi);
}
function id() { return POOL[ri(POOL.length)]; }
function num() { return "" + ri(100000); }
function expr(depth) {
  var r = ri(10);
  if (depth <= 0 || r < 4) { return (r & 1) ? id() : num(); }
  var ops = "+-*/";
  var s = expr(depth - 1) + " " + ops[ri(4)] + " " + expr(depth - 1);
  if (r >= 8) s = "(" + s + ")";
  return s;
}
function strlit() {
  var n = 3 + ri(12), s = "";
  for (var i = 0; i < n; i++) {
    var r = ri(20);
    if (r === 0) s += "\\n";
    else if (r === 1) s += "\\\"";
    else if (r === 2) s += "\\\\";
    else s += LETTERS[ri(26)];
  }
  return s;
}
var parts = [], len = 0;
while (len < 2000000) {
  var t = ri(10), line;
  if (t < 4) line = "var " + id() + " = " + expr(3) + ";\n";
  else if (t < 5) line = "function " + id() + "(" + id() + ", " + id() + ") { return " + id() + " + " + num() + " * " + id() + "; }\n";
  else if (t < 6) line = "// note " + id() + " " + num() + "\n";
  else if (t < 7) line = "if (" + id() + " <= " + num() + " && " + id() + " === " + num() + ") { " + id() + "++; } else { " + id() + " = 0; }\n";
  else if (t < 8) line = id() + "." + id() + " = \"" + strlit() + "\";\n";
  else if (t < 9) line = id() + "(" + id() + ", " + num() + ", \"" + strlit() + "\");\n";
  else line = "/* " + id() + " block " + num() + " */ var " + id() + " = " + id() + " => " + id() + " * 2;\n";
  parts.push(line);
  len += line.length;
}
var src = parts.join("");

// ---- tokenizer ----
// kinds: 1 ident, 2 number, 3 string, 4 punct, 5 comment
var kinds, starts, ends;
function tokenize() {
  kinds = []; starts = []; ends = [];
  var i = 0, n = src.length, d;
  while (i < n) {
    var c = src.charCodeAt(i);
    if (c === 32 || c === 10 || c === 9 || c === 13) { i++; continue; }
    var st = i;
    if (c === 47) { // '/'
      var cc = src.charCodeAt(i + 1);
      if (cc === 47) { // line comment
        i += 2;
        while (i < n && src.charCodeAt(i) !== 10) i++;
        kinds.push(5); starts.push(st); ends.push(i); continue;
      }
      if (cc === 42) { // block comment
        i += 2;
        while (i < n && !(src.charCodeAt(i) === 42 && src.charCodeAt(i + 1) === 47)) i++;
        i += 2;
        kinds.push(5); starts.push(st); ends.push(i); continue;
      }
    }
    if ((c >= 97 && c <= 122) || (c >= 65 && c <= 90) || c === 95 || c === 36) { // identifier
      i++;
      while (i < n) {
        d = src.charCodeAt(i);
        if ((d >= 97 && d <= 122) || (d >= 65 && d <= 90) || (d >= 48 && d <= 57) || d === 95 || d === 36) i++;
        else break;
      }
      kinds.push(1); starts.push(st); ends.push(i); continue;
    }
    if (c >= 48 && c <= 57) { // number: digits [. digits] [e[+-]digits]
      i++;
      while (i < n && (d = src.charCodeAt(i)) >= 48 && d <= 57) i++;
      if (src.charCodeAt(i) === 46) {
        i++;
        while (i < n && (d = src.charCodeAt(i)) >= 48 && d <= 57) i++;
      }
      d = src.charCodeAt(i);
      if (d === 101 || d === 69) {
        i++;
        d = src.charCodeAt(i);
        if (d === 43 || d === 45) i++;
        while (i < n && (d = src.charCodeAt(i)) >= 48 && d <= 57) i++;
      }
      kinds.push(2); starts.push(st); ends.push(i); continue;
    }
    if (c === 34 || c === 39) { // string with escapes
      var q = c;
      i++;
      while (i < n) {
        d = src.charCodeAt(i);
        if (d === 92) { i += 2; continue; }
        i++;
        if (d === q) break;
      }
      kinds.push(3); starts.push(st); ends.push(i); continue;
    }
    // punctuators: longest match 3 -> 2 -> 1
    var c1 = src.charCodeAt(i + 1), c2 = src.charCodeAt(i + 2);
    if ((c === 61 && c1 === 61 && c2 === 61) || (c === 33 && c1 === 61 && c2 === 61)) i += 3;
    else if ((c === 61 && c1 === 61) || (c === 33 && c1 === 61) || (c === 60 && c1 === 61) ||
             (c === 62 && c1 === 61) || (c === 38 && c1 === 38) || (c === 124 && c1 === 124) ||
             (c === 61 && c1 === 62) || (c === 43 && c1 === 43) || (c === 45 && c1 === 45) ||
             (c === 43 && c1 === 61) || (c === 45 && c1 === 61)) i += 2;
    else i += 1;
    kinds.push(4); starts.push(st); ends.push(i);
  }
}

// rolling FNV-1a over (kind, length, first char) of every token
var h = 0x811c9dc5 >>> 0;
function mix(x) { h = Math.imul(h ^ x, 16777619) >>> 0; }

// ---- recursive-descent expression parser over `var <id> = ... ;` spans ----
var P_pos = 0, P_end = 0, P_nodes = 0, P_ok = true;
function tokIs(i, ch) {
  return kinds[i] === 4 && ends[i] - starts[i] === 1 && src.charCodeAt(starts[i]) === ch;
}
function pFactor() {
  if (P_pos >= P_end) { P_ok = false; return; }
  var k = kinds[P_pos];
  if (k === 1 || k === 2) { P_pos++; P_nodes++; return; }
  if (tokIs(P_pos, 40)) { // (
    P_pos++;
    pExpr();
    if (tokIs(P_pos, 41)) P_pos++; else P_ok = false;
    return;
  }
  if (tokIs(P_pos, 45)) { P_pos++; P_nodes++; pFactor(); return; } // unary -
  P_ok = false;
}
function pTerm() {
  pFactor();
  while (P_ok && P_pos < P_end && (tokIs(P_pos, 42) || tokIs(P_pos, 47))) {
    P_pos++; P_nodes++; pFactor();
  }
}
function pExpr() {
  pTerm();
  while (P_ok && P_pos < P_end && (tokIs(P_pos, 43) || tokIs(P_pos, 45))) {
    P_pos++; P_nodes++; pTerm();
  }
}
var parsedExprs = 0, nodeCount = 0;
var ROUNDS = 8;
for (var round = 0; round < ROUNDS; round++) {
  tokenize();
  for (var ti = 0; ti < kinds.length; ti++) {
    mix(kinds[ti]); mix(ends[ti] - starts[ti]); mix(src.charCodeAt(starts[ti]));
  }
  for (var i = 0; i + 3 < kinds.length; i++) {
    // match: ident("var") ident '=' ... ';'
    if (kinds[i] === 1 && ends[i] - starts[i] === 3 &&
        src.charCodeAt(starts[i]) === 118 && src.charCodeAt(starts[i] + 1) === 97 && src.charCodeAt(starts[i] + 2) === 114 &&
        kinds[i + 1] === 1 && tokIs(i + 2, 61)) {
      var j = i + 3;
      while (j < kinds.length && !tokIs(j, 59)) j++;
      P_pos = i + 3; P_end = j; P_nodes = 0; P_ok = true;
      pExpr();
      if (P_ok && P_pos === P_end) { parsedExprs++; nodeCount += P_nodes; }
      i = j;
    }
  }
}
console.log("srcLen=" + src.length + " tokens=" + kinds.length + " hash=" + h + " parsedExprs=" + parsedExprs + " nodes=" + nodeCount);
