// finally-labels: non-linear control flow without (mostly) throwing.
//
// Measures try/finally on the normal path: return from inside try (the
// return value is fixed before finally mutates locals), nested finally
// chains, break and continue that leave a try block inside nested loops,
// labelled break/continue across loop nests and a labelled block, switch on
// strings and on small dense ints with fallthrough and a default case in the
// middle, a sparse-int switch, a char-code lexer (switch plus labelled
// continue), and a smaller share of Error construction (an Error subclass and
// Error with `cause`) caught and rethrown two and three levels up.
//
// Why a likely weak spot: finally on a non-exceptional exit needs a
// completion record (return value, break target, or continue target) to be
// saved and resumed; interpreters often lower this through a generic slow
// path, a gosub/ret pair, or by duplicating code. Labelled jumps that leave
// several scopes and switch dispatch on string keys are also handled
// differently by every engine. The throwing share is kept small because
// bench/hostile/errors already covers plain throw/catch.
//
// Dominant operations: try/finally entry and exit, completion dispatch,
// string comparison in switch, jump tables, Error object construction
// (including stack capture) and instanceof.
"use strict";

const SIZE = 100;

let seed = 7;
function rndInt(n) {
  seed = (Math.imul(seed, 1664525) + 1013904223) | 0;
  return ((seed >>> 0) % n) | 0;
}

let fin = 0;

// --- try/finally on the normal path ----------------------------------------------
function returnThroughFinally(x) {
  try {
    if (x & 1) return x * 3;
    if (x & 2) return x + 7;
  } finally {
    fin++;
  }
  return x;
}
function nestedFinally(x) {
  let s = x;
  try {
    try {
      s = (s + 1) | 0;
      if (s & 4) return s;          // value captured before both finally blocks run
    } finally {
      s ^= 0x55;
      fin++;
    }
    s = (s + 2) | 0;
  } finally {
    s = (s * 3) | 0;
    fin++;
  }
  return s;
}
function finallyOverride(x) {
  // a return inside finally replaces the pending return value
  try {
    return x;
  } finally {
    fin++;
    if ((x & 7) === 0) return -x;    // eslint-disable-line no-unsafe-finally
  }
}
function loopsThroughFinally(n, seedVal) {
  let acc = seedVal;
  for (let i = 0; i < n; i++) {
    for (let j = 0; j < 16; j++) {
      try {
        if (j === (i & 15)) continue;
        if (j > 12) break;
        acc = (acc + i * j) | 0;
      } finally {
        acc ^= j;
        fin++;
      }
    }
  }
  return acc;
}
function searchWithCleanup(rows, target) {
  let opened = 0, closed = 0, hit = -1;
  scan: for (let r = 0; r < rows.length; r++) {
    opened++;
    try {
      const row = rows[r];
      for (let c = 0; c < row.length; c++) {
        if (row[c] < 0) continue scan;          // leaves try: finally runs
        if (row[c] === target) { hit = r * 100 + c; break scan; }
      }
    } finally {
      closed++;
    }
  }
  return (hit + opened * 7 + closed * 13) | 0;
}

// --- labelled break / continue ----------------------------------------------------------
function labelledGrid(grid, w, h) {
  let found = 0, sum = 0, skippedRows = 0;
  outer: for (let y = 0; y < h; y++) {
    inner: for (let x = 0; x < w; x++) {
      const v = grid[y * w + x];
      if (v === 0) { skippedRows++; continue outer; }
      if (v === 1) continue inner;
      if (v === 255) break outer;
      for (let k = 0; k < 3; k++) {
        if (((v >> k) & 1) === 0) continue inner;
      }
      sum = (sum + v) | 0;
      found++;
    }
  }
  check: {
    if (found === 0) break check;
    sum = (sum + found) | 0;
    if (skippedRows > 3) break check;
    sum ^= 0xabc;
  }
  return (sum + skippedRows * 31) | 0;
}
function triangleSearch(n) {
  let count = 0;
  a: for (let i = 1; i < n; i++) {
    b: for (let j = i; j < n; j++) {
      for (let k = j; k < n; k++) {
        const s = i * i + j * j;
        if (k * k > s) continue b;
        if (k * k === s) { count += i + j + k; continue a; }
      }
    }
  }
  return count;
}

// --- switch statements --------------------------------------------------------------------
const KEYWORDS = ["let", "const", "var", "function", "if", "else", "for", "while", "do",
  "break", "continue", "return", "switch", "case", "default", "class", "new", "typeof", "x", "value"];
function classify(tok) {
  let score = 0;
  switch (tok) {
    case "let": case "const": case "var": score += 1;      // fallthrough
    case "function": score += 2; break;
    case "if": case "else": score += 3; break;
    case "for": case "while": case "do": score += 4;       // fallthrough
    case "break": case "continue": score += 5; break;
    case "return": score += 6; break;
    default: score += tok.length;                          // fallthrough into a later case
    case "switch": score += 7; break;
    case "case": score += 8;
  }
  return score;
}
function denseOp(op, a, b) {
  switch (op) {
    case 0: return (a + b) | 0;
    case 1: a = (a * 2) | 0;                                // fallthrough
    case 2: return (a - b) | 0;
    case 3: case 4: return a ^ b;
    case 5: b = b & 31;                                     // fallthrough
    case 6: return a << (b & 15);
    case 7: return a >> (b & 15);
    case 8: return Math.imul(a, b);
    case 9: case 10: case 11: return (a | b) & 0xffff;
    case 12: a = -a;                                        // fallthrough
    case 13: return (a + 1) | 0;
    case 14: return a & b;
    default: return (a + op) | 0;
  }
}
function sparseOp(code, x) {
  switch (code) {
    case -1: return x - 1;
    case 3: return x + 3;
    case 100: return x ^ 100;
    case 1000: return x * 2;
    case 4096: return x >> 1;
    case 65535: return x & 65535;
    case 1 << 20: return x + 20;
    default: return x;
  }
}
// small lexer: switch on char codes with labelled continue
function lex(src) {
  let i = 0, idents = 0, numbers = 0, puncts = 0, hash = 0;
  const n = src.length;
  loop: while (i < n) {
    const c = src.charCodeAt(i);
    switch (c) {
      case 32: case 9: case 10: i++; continue loop;
      case 40: case 41: case 123: case 125: case 59: case 44: puncts++; i++; continue loop;
      case 61: case 43: case 45: case 42:
        puncts++; i++;
        if (i < n && src.charCodeAt(i) === 61) i++;
        continue loop;
    }
    if (c >= 48 && c <= 57) {
      let v = 0;
      while (i < n) {
        const d = src.charCodeAt(i);
        if (d < 48 || d > 57) break;
        v = (v * 10 + d - 48) | 0;
        i++;
      }
      numbers++;
      hash = (hash + v) | 0;
      continue;
    }
    const start = i;
    while (i < n) {
      const d = src.charCodeAt(i);
      if (!((d >= 97 && d <= 122) || (d >= 65 && d <= 90) || d === 95)) break;
      i++;
    }
    if (i === start) { i++; continue; }
    idents++;
    hash = (Math.imul(hash, 31) + classify(src.slice(start, i))) | 0;
  }
  return (hash + idents * 3 + numbers * 5 + puncts * 7) | 0;
}

// --- Error construction, catch and rethrow a few levels up ----------------------
class ParseError extends Error {
  constructor(message, pos) {
    super(message);
    this.name = "ParseError";
    this.pos = pos;
  }
}
function level3(x) {
  if ((x % 41) === 0) throw new ParseError("bad token " + (x & 15), x);
  return x & 255;
}
function level2(x) {
  try {
    return level3(x) + 1;
  } catch (e) {
    if (e instanceof ParseError && (e.pos & 1)) { e.pos += 1; throw e; }   // rethrow same object
    return -1;
  }
}
function level1(x) {
  try {
    return level2(x);
  } catch (e) {
    throw new Error("wrapped: " + e.message, { cause: e });               // wrap and rethrow
  } finally {
    fin++;
  }
}
function topLevel(x) {
  try {
    return level1(x);
  } catch (e) {
    return (e.message.length + e.cause.pos + (e.cause instanceof ParseError ? 1000 : 0)) | 0;
  }
}

// --- driver --------------------------------------------------------------------------------
const grids = [];
for (let g = 0; g < 8; g++) {
  const grid = new Array(64 * 64);
  for (let i = 0; i < grid.length; i++) grid[i] = rndInt(3000) === 0 ? 255 : rndInt(160 + g * 10);
  grids.push(grid);
}
const rows = [];
let srcText = "";
for (let i = 0; i < 400; i++) {
  srcText += KEYWORDS[i % KEYWORDS.length] + " v" + (i % 13) + " = " + (i * 7919 % 10007) + (i & 1 ? "; " : " + (x) {\n");
}
let finHash = 0, labelHash = 0, switchHash = 0, errHash = 0;

for (let round = 0; round < SIZE; round++) {
  // try/finally without throwing
  for (let k = 0; k < 6000; k++) {
    const x = rndInt(1 << 16);
    finHash = (finHash + returnThroughFinally(x) + nestedFinally(x) + finallyOverride(x)) | 0;
  }
  finHash = (finHash ^ loopsThroughFinally(1200, round)) | 0;
  rows.length = 0;
  for (let r = 0; r < 40; r++) {
    const row = [];
    for (let c = 0; c < 20; c++) row.push(rndInt(50) - 1);
    rows.push(row);
  }
  for (let t = 0; t < 30; t++) finHash = (finHash + searchWithCleanup(rows, t)) | 0;

  // labelled break / continue
  for (let g = 0; g < 6; g++) {
    const grid = grids[(round + g) & 7];
    grid[(round * 61 + g) & 4095] = (round + g) & 255;   // keep each pass distinct
    labelHash = (labelHash + labelledGrid(grid, 64, 64)) | 0;
  }
  labelHash = (labelHash ^ triangleSearch(60 + (round & 7))) | 0;

  // switch on strings and on ints
  for (let k = 0; k < 4000; k++) {
    switchHash = (switchHash + classify(KEYWORDS[rndInt(KEYWORDS.length)])) | 0;
    switchHash = (switchHash ^ denseOp(rndInt(18), k, round + k)) | 0;
    switchHash = (switchHash + sparseOp(k & 1 ? 1000 : (k & 6) === 2 ? 65535 : k & 7, k)) | 0;
  }
  switchHash = (switchHash + lex(srcText)) | 0;

  // Error construction plus rethrow (about one value in 41 throws)
  for (let k = 0; k < 1000; k++) errHash = (errHash + topLevel(round * 1000 + k)) | 0;
}

console.log("finally-labels rounds=" + SIZE + " finallyRuns=" + fin);
console.log("finally=" + finHash + " labels=" + labelHash);
console.log("switch=" + switchHash + " errors=" + errHash);
