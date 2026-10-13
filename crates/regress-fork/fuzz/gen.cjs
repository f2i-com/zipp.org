// Random ECMAScript regular expressions and subjects for the differential
// fuzzer (fuzz.cjs). `generate(seed, count)` returns [pattern, flags,
// subject, lastIndex] cases; the same seed always gives the same cases.
//
// Patterns follow the grammar of ECMA-262 22.2.1 (and Annex B for patterns
// without `u`/`v`), with every flag combination, and some deliberate
// mistakes so the error paths are compared too. Subjects mix the pattern's
// own characters with ASCII, Latin-1, BMP letters with special case folds,
// astral characters and lone surrogates.
'use strict';

function rng(seed) {
  let a = seed | 0;
  const next = () => {
    a = (a + 0x6D2B79F5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  const r = {
    f: next,
    int: n => (next() * n) | 0,
    pick: xs => xs[(next() * xs.length) | 0],
    chance: p => next() < p,
  };
  return r;
}

// Characters a pattern may contain literally, and subjects draw from.
const cps = xs => xs.map(c => String.fromCodePoint(c));
// Characters a pattern may contain literally, and subjects draw from: ASCII,
// then Latin-1 and BMP letters with special case folds (U+017F, U+212A, the
// sharp s, the three sigmas, dotted/dotless i, U+2028, the micro sign, a
// titlecase digraph) and astral letters with case pairs.
const LIT_ASCII = ['a', 'b', 'c', 'x', 'A', 'B', 'K', 's', 'S', 'k', '0', '1', '9', ' ', '-', '_', ',', '\n'];
const LIT = LIT_ASCII.concat(cps([0xE9, 0xC9, 0x17F, 0x212A, 0xDF, 0x1E9E, 0x3A3, 0x3C3, 0x3C2, 0x130, 0x131,
  0x2028, 0xB5, 0x39C, 0x1C5, 0x1F600, 0x1D400, 0x10400, 0x10428]));
// Lone surrogates, other controls and punctuation, only in subjects.
const SUBJ_EXTRA = [String.fromCharCode(0xD83D), String.fromCharCode(0xDE00), String.fromCharCode(0xD835),
  String.fromCharCode(0xDC00), '\r', '\t', String.fromCharCode(0xA0), String.fromCharCode(0xFEFF),
  '.', '$', '(', ')', '*', 'z', 'Z', 'y'];
const SUBJ_EXTRA_ASCII = ['\r', '\t', '.', '$', '(', ')', '*', 'z', 'Z', 'y', '<', '>', '/', ':', '='];
const PROPS = ['L', 'Lu', 'Ll', 'N', 'Nd', 'P', 'S', 'ASCII', 'Any', 'Assigned', 'Alphabetic', 'White_Space',
  'Script=Greek', 'Script=Latin', 'sc=Cyrillic', 'Script_Extensions=Latin', 'scx=Grek', 'General_Category=Letter',
  'gc=Lt', 'Lowercase', 'Uppercase', 'ID_Start', 'Emoji', 'Emoji_Presentation', 'Changes_When_Casefolded',
  'Script=Unknown', 'Cased', 'Case_Ignorable', 'Lt', 'Ideographic', 'Hex_Digit'];
const BAD_PROPS = ['Foo', 'L=x', 'Script', 'Script=Bogus', 'lu', 'ascii', 'General_Category', 'Block=Basic_Latin', 'RGI_Emoji'];
const STR_PROPS = ['RGI_Emoji', 'Basic_Emoji', 'Emoji_Keycap_Sequence', 'RGI_Emoji_Flag_Sequence'];
const NAMES = ['n1', 'n2', 'x', 'nm'];

function hex(n, w) { return n.toString(16).toUpperCase().padStart(w, '0'); }

function makeGen(r, flags, LIT, long) {
  const u = flags.includes('u'), v = flags.includes('v'), um = u || v;
  let groups = 0;
  const names = [];
  const literals = [];

  function litChar() {
    const c = r.pick(LIT);
    literals.push(c);
    if (c === '\n') return r.chance(0.5) ? '\\n' : '\n';
    return c;
  }

  function charEscape() {
    switch (r.int(14)) {
      case 0: return r.pick(['\\t', '\\n', '\\r', '\\v', '\\f']);
      case 1: return '\\c' + r.pick(['A', 'j', 'M', 'z', '1', '_']);
      case 2: return '\\0';
      case 3: { const c = r.pick(LIT).codePointAt(0); literals.push(String.fromCodePoint(c)); return c < 256 ? '\\x' + hex(c, 2) : '\\u' + hex(c & 0xffff, 4); }
      case 4: { const c = r.pick(LIT).codePointAt(0); literals.push(String.fromCodePoint(c)); return '\\u{' + hex(c, r.int(3) + 1) + '}'; }
      case 5: return r.pick(['\\uD83D\\uDE00', '\\uD835\\uDC00', '\\uD83D', '\\uDE00', '\\u{D83D}', '\\u{1F600}']);
      case 6: return '\\' + r.pick(['.', '*', '+', '?', '(', ')', '[', ']', '{', '}', '|', '^', '$', '/', '\\', '-']);
      case 7: return '\\' + r.pick(['a', 'e', 'z', '_', '8', '9', 'M', 'q', 'y', 'k']);
      case 8: return '\\' + r.pick(['01', '07', '12', '377', '400', '0a']);
      case 9: return '\\x' + r.pick(['4', 'G1', '']);
      case 10: return '\\u' + r.pick(['00', '{110000}', '{', 'D83D\\u0041']);
      default: return '\\' + r.pick(['d', 'D', 'w', 'W', 's', 'S']);
    }
  }

  function propEscape() {
    const neg = r.chance(0.3);
    let p;
    if (r.chance(0.08)) p = r.pick(BAD_PROPS);
    else if (v && !neg && r.chance(0.15)) p = r.pick(STR_PROPS);
    else p = r.pick(PROPS);
    return '\\' + (neg ? 'P' : 'p') + '{' + p + '}';
  }

  function classAtom() {
    switch (r.int(8)) {
      case 0: case 1: case 2: return litChar();
      case 3: return '\\' + r.pick(['d', 'D', 'w', 'W', 's', 'S']);
      case 4: return r.chance(0.5) ? '\\b' : '\\-';
      case 5: return um && r.chance(0.6) ? propEscape() : charEscape();
      case 6: return r.pick(['-', '^', '[', ']', '$', '.', '|']);
      default: return charEscape();
    }
  }

  function legacyClass() {
    let s = '[' + (r.chance(0.3) ? '^' : '');
    const n = r.int(4);
    for (let i = 0; i < n; i++) {
      if (r.chance(0.35)) s += classAtom() + '-' + classAtom();
      else s += classAtom();
    }
    return s + ']';
  }

  function vOperand(d) {
    switch (r.int(6)) {
      case 0: return d > 0 ? vClass(d - 1) : litChar();
      case 1: return '\\q{' + Array.from({ length: 1 + r.int(3) }, () => Array.from({ length: r.int(3) }, litChar).join('')).join('|') + '}';
      case 2: return propEscape();
      case 3: return '\\' + r.pick(['d', 'w', 's', 'W']);
      default: return litChar();
    }
  }

  function vClass(d) {
    let s = '[' + (r.chance(0.25) ? '^' : '');
    const kind = r.int(4);
    if (kind === 0) {
      s += vOperand(d) + '--' + vOperand(d);
    } else if (kind === 1) {
      s += vOperand(d) + '&&' + vOperand(d);
    } else {
      const n = r.int(4);
      for (let i = 0; i < n; i++) {
        if (r.chance(0.3)) s += litChar() + '-' + litChar();
        else s += vOperand(d);
      }
      if (r.chance(0.05)) s += r.pick(['(', '--', '&&&', '{', '|']);
    }
    return s + ']';
  }

  function group(d) {
    switch (r.int(9)) {
      case 0: case 1: { groups++; return '(' + disj(d - 1) + ')'; }
      case 2: {
        groups++;
        const nm = r.pick(NAMES);
        names.push(nm);
        return '(?<' + nm + '>' + disj(d - 1) + ')';
      }
      case 3: return '(?:' + disj(d - 1) + ')';
      case 4: return '(?' + r.pick(['=', '!', '<=', '<!']) + disj(d - 1) + ')';
      case 5: {
        const on = r.pick(['i', 'm', 's', 'is', '', 'ims', 'ii']);
        const off = r.chance(0.5) ? '-' + r.pick(['i', 'm', 's', '', 'i']) : '';
        return '(?' + on + off + ':' + disj(d - 1) + ')';
      }
      default: { groups++; return '(' + disj(d - 1) + ')'; }
    }
  }

  function atom(d) {
    const k = r.int(20);
    if (k < 7) return litChar();
    if (k < 8) return '.';
    if (k < 10) return charEscape();
    if (k < 11) return um ? propEscape() : charEscape();
    if (k < 13) return v ? vClass(1) : legacyClass();
    if (k < 16 && d > 0) return group(d);
    if (k < 17) {
      if (names.length && r.chance(0.5)) return '\\k<' + r.pick(names) + '>';
      return '\\' + (1 + r.int(Math.max(1, groups + 1)));
    }
    if (k < 18 && r.chance(0.4)) return r.pick(['(', ')', '[', ']', '{', '}', '*', '+', '?', '{1', 'a{2,1}', '\\', '(?<>a)', '(?<1a>a)', '\\k<zz>', '\\k']);
    return litChar();
  }

  function quant() {
    const k = r.int(14);
    let q;
    if (k < 8) return '';
    if (k < 9) q = '*';
    else if (k < 10) q = '+';
    else if (k < 11) q = '?';
    else if (k < 12) q = '{' + r.int(3) + '}';
    else if (k < 13) q = '{' + r.int(3) + ',}';
    else { const a = r.int(3); q = '{' + a + ',' + (a + r.int(3)) + '}'; }
    return q + (r.chance(0.3) ? '?' : '');
  }

  function term(d) {
    const k = r.int(14);
    if (k === 0) return r.pick(['^', '$', '\\b', '\\B']);
    const a = atom(d);
    // A pattern for a long subject quantifies no group: nested quantifiers
    // over hundreds of characters can take exponential time in Node.
    if (long && a[0] === '(') return a;
    // Lookaheads are quantifiable only without u/v (Annex B); emit it anyway
    // sometimes, for the error.
    return a + quant();
  }

  function alt(d) {
    const n = d === 3 ? 1 + r.int(5) : r.int(4);
    let s = '';
    for (let i = 0; i < n; i++) s += term(d);
    return s;
  }

  function disj(d) {
    const n = r.chance(0.7) ? 1 : 2 + r.int(2);
    const xs = [];
    for (let i = 0; i < n; i++) xs.push(alt(d));
    return xs.join('|');
  }

  return { pattern: () => disj(3), literals };
}

const FLAG_SETS = ['', 'g', 'i', 'm', 's', 'y', 'u', 'v', 'gi', 'iu', 'iv', 'gu', 'gv', 'yu', 'ms', 'ims', 'gimsuy',
  'gimsvy', 'd', 'dg', 'dgu', 'iy', 'imu', 'su', 'msv', 'dv', 'uv', 'gg', 'x'];

function subject(r, lits, alphabet, extra, long) {
  const pick = () => {
    const k = r.f();
    if (k < 0.55) return r.pick(lits);
    if (k < 0.85) return r.pick(alphabet);
    return r.pick(extra);
  };
  let s = '';
  if (long) {
    // A long subject: runs of one character and short random stretches, so
    // loops, start searches and global scans have something to walk.
    const len = 100 + r.int(900);
    while (s.length < len) {
      if (r.chance(0.5)) s += pick().repeat(1 + r.int(30));
      else for (let j = 0, n = 1 + r.int(12); j < n; j++) s += pick();
    }
    return s;
  }
  for (let j = 0, n = r.int(17); j < n; j++) s += pick();
  return s;
}

function generate(seed, count) {
  const r = rng(seed);
  const out = [];
  for (let i = 0; i < count; i++) {
    let flags = r.pick(FLAG_SETS);
    if (flags !== 'uv' && flags !== 'gg' && flags !== 'x' && r.chance(0.2)) flags = flags + r.pick(['g', 'y', 'd', 'i']);
    if (/(.).*\1/.test(flags) && r.chance(0.9)) flags = [...new Set(flags)].join('');
    // Four cases in ten are ASCII only, pattern literals and subject: those
    // run the one-byte matcher, its start searches and the native JIT.
    const ascii = r.chance(0.4);
    const alphabet = ascii ? LIT_ASCII : LIT;
    const long = r.chance(0.12);
    const g = makeGen(r, flags, alphabet, long);
    const pattern = g.pattern();
    const lits = g.literals.length ? g.literals : ['a'];
    const s = subject(r, ascii ? lits.filter(c => c.charCodeAt(0) < 128).concat(['a']) : lits, alphabet,
      ascii ? SUBJ_EXTRA_ASCII : SUBJ_EXTRA, long);
    let li = 0;
    if (r.chance(0.35)) li = r.int(s.length + 2);
    out.push([pattern, flags, s, li]);
  }
  return out;
}

module.exports = { generate, rng };
