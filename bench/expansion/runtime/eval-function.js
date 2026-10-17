// eval-function: compiling generated code at run time.
//
// Measures the compile-and-run path for dynamic code after the engine is
// warm: (A) `new Function` over thousands of small, textually distinct
// bodies (expressions, loops, branches, object literals), each called a few
// times; (B) indirect eval `(0, eval)(src)` of generated function
// expressions and of plain expressions; (C) one large generated function of
// a few thousand lines (few locals, many statements, branches and a switch)
// compiled once and called repeatedly; (D) direct eval of short expressions
// inside a function scope, reading and writing its locals. Phase D mixes a
// small set of repeated source strings (cache-friendly: an engine with an
// eval cache keyed by source text may skip recompiling) with strings that
// are distinct on every call.
//
// Why a likely weak spot: the lexer, parser and bytecode emitter run on the
// hot path; direct eval also forces the enclosing scope to be materialised
// so eval code can reach its locals, and a large one-shot function exposes
// compile-time cost per line and any super-linear behaviour in the emitter.
//
// Dominant operations: tokenising and parsing, scope analysis, bytecode
// generation, function object creation, then short execution.
"use strict";

const SIZE = 100;

let seed = 31337;
function rndInt(n) {
  seed = (Math.imul(seed, 1664525) + 1013904223) | 0;
  return (((seed >>> 8) & 0xffffff) % n) | 0;
}

// warm-up: ordinary code first, so dynamic compilation happens in a warm engine
let warm = 0;
for (let i = 0; i < 20000; i++) warm = (warm + Math.imul(i, i ^ 5)) | 0;

const OPS = ["+", "-", "^", "|", "&"];
let hashA = 0, hashB = 0, hashC = 0, hashD = 0, compiled = 0;

// --- (A) many small distinct functions via new Function ---------------------------------
function smallBody(i) {
  const c1 = (i * 7) % 97, c2 = i % 13, op = OPS[i % OPS.length];
  switch (i & 3) {
    case 0:
      return "return ((a " + op + " " + c1 + ") * b + " + c2 + ") | 0;";
    case 1:
      return "let s = " + c2 + "; for (let i = 0; i < b; i++) { s = (s " + op + " i * " + c1 + ") | 0; } return s;";
    case 2:
      return "if (a > " + c1 + ") { return (a - b) " + op + " " + c2 + "; } else if (a === " + c2 + ") { return " + i +
        "; } return (b << " + (c2 & 7) + ") | 0;";
    default:
      return "const o = { x: a, y: b, k" + (i & 31) + ": " + c1 + " }; return (o.x " + op + " o.y + o.k" + (i & 31) + ") | 0;";
  }
}
for (let i = 0; i < SIZE * 150; i++) {
  const f = new Function("a", "b", smallBody(i));
  compiled++;
  for (let k = 0; k < 3; k++) hashA = (hashA + f(k + (i & 15), (i & 7) + k)) | 0;
}

// --- (B) indirect eval of generated function expressions and expressions -------------
const geval = eval;
for (let i = 0; i < SIZE * 80; i++) {
  const k = i % 17, j = (i * 13) % 101;
  const g = geval("(function (x, y) { var t = x * " + k + " + y; return (t " + OPS[i % 5] + " " + j + ") | 0; })");
  compiled++;
  hashB = (hashB + g(i & 255, 3) + g(7, i & 31)) | 0;
  hashB = (hashB ^ (0, eval)("(" + i + " * 3 + " + k + ") % 1000")) | 0;
}

// --- (C) one large generated function, compiled once, called repeatedly -------------
const LINES = Math.min(3000, SIZE * 30);
function bigSource(lines) {
  const out = ["let a = x | 0, b = y | 0, c = 7, d = 0;"];
  for (let i = 0; i < lines; i++) {
    const r = rndInt(8), k = 1 + rndInt(97);
    switch (r) {
      case 0: out.push("a = (a + Math.imul(b, " + k + ")) | 0;"); break;
      case 1: out.push("b = (b ^ (a >>> " + (k & 15) + ")) | 0;"); break;
      case 2: out.push("if ((a & " + (k & 7) + ") === 1) { c = (c + " + k + ") | 0; } else { d = (d - c) | 0; }"); break;
      case 3: out.push("d = (d + (b & " + k + ") - (c | 1)) | 0;"); break;
      case 4: out.push("c = (c * " + (k & 7) + " + a) & 65535;"); break;
      case 5:
        out.push("switch (b & 3) { case 0: a = (a + " + k + ") | 0; break; case 1: b = (b - " + k +
          ") | 0; break; case 2: c ^= " + k + "; break; default: d = (d + 1) | 0; }");
        break;
      case 6: out.push("for (let i = 0; i < " + (1 + (k & 3)) + "; i++) { a = (a + i * c) | 0; }"); break;
      default: out.push("b = ((b << 1) | (a & 1)) | 0;"); break;
    }
  }
  out.push("return (a ^ b ^ c ^ d) | 0;");
  return out.join("\n");
}
const big = new Function("x", "y", bigSource(LINES));
compiled++;
for (let i = 0; i < SIZE * 3; i++) hashC = (hashC + big(i, hashC & 1023)) | 0;

// --- (D) direct eval of short expressions in a function scope -----------------------------
function scoped(a, b) {
  let c = a ^ b, s = 0;
  for (let i = 0; i < 8; i++) {
    s = (s + eval("a + b * " + (i & 3) + " - c")) | 0;          // four repeated sources
  }
  s = (s + eval("c * 2 + " + a + " - " + b)) | 0;               // distinct per call
  eval("c = c + 1");                                            // write a local through eval
  return (s + c) | 0;
}
for (let i = 0; i < SIZE * 50; i++) hashD = (hashD + scoped(i & 1023, (i * 7) & 255)) | 0;

console.log("eval-function size=" + SIZE + " compiled=" + compiled + " bigLines=" + LINES + " warm=" + warm);
console.log("A=" + hashA + " B=" + hashB + " C=" + hashC + " D=" + hashD);
