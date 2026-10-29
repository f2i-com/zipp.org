// getGlobalsBatch carries a batch JSON can spell as one JSON text, and
// anything else property by property. Both must give the host the same values:
// these pin what the text path builds (strings needing escapes, numbers at the
// edges of their spelling, -0, an own `__proto__`) and that a batch holding a
// value JSON cannot carry (undefined, NaN, Infinity, a lone surrogate) still
// arrives whole, wherever in the batch that value sits.
"use strict";
const { Engine } = require("./pkg/zipp_wasm.js");

let pass = 0, fail = 0;
function check(label, ok, detail) {
  if (ok) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label}${detail ? " — " + detail : ""}`); }
}

// Deep equality that tells -0 from 0 and NaN from NaN's absence, and an
// array hole or an undefined element from a missing one.
function same(a, b) {
  if (typeof a !== typeof b) return false;
  if (a === null || b === null || typeof a !== "object") return Object.is(a, b);
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  if (Object.getPrototypeOf(a) !== Object.getPrototypeOf(b)) return false;
  const ka = Reflect.ownKeys(a), kb = Reflect.ownKeys(b);
  if (ka.length !== kb.length) return false;
  for (let i = 0; i < ka.length; i++) {
    if (ka[i] !== kb[i]) return false;
    const da = Object.getOwnPropertyDescriptor(a, ka[i]);
    const db = Object.getOwnPropertyDescriptor(b, kb[i]);
    if (da.enumerable !== db.enumerable || da.writable !== db.writable || da.configurable !== db.configurable) return false;
    if (!same(da.value, db.value)) return false;
  }
  return true;
}

const e = new Engine();
const syms = e.initScript(`
  var text = { quote: 'say "hi"', slash: "a\\\\b", lines: "one\\ntwo\\tthree\\r", control: "\\u0001\\u001f",
               wide: "é中😀", sep: "  ", empty: "" };
  var numbers = [0, -0, 1, -1, 0.1 + 0.2, 1e21, 1e-7, 123456789012345680000, 5e-324, 1.7976931348623157e308,
                 2 ** 53, -(2 ** 53) - 2, 1e-6, 999999999999999900000];
  var shapes = { a: [1, [2, [3, {}]], []], b: { c: { d: null } }, t: true, f: false, n: null, "1": "index", "": "blank" };
  var proto = JSON.parse('{"__proto__": {"x": 1}, "y": 2}');
  var holey = [1, undefined, 3];
  var notnumbers = { nan: NaN, inf: Infinity, ninf: -Infinity };
  var lone = "a\\ud800b";
  var nested = { deep: [{ v: undefined }] };
`);
if (syms && syms.error) throw new Error("initScript: " + syms.error);
const ix = (n) => syms[n].index;

const [text, numbers, shapes, proto] = e.getGlobalsBatch([ix("text"), ix("numbers"), ix("shapes"), ix("proto")]);
check("strings needing escapes arrive as written", same(text, {
  quote: 'say "hi"', slash: "a\\b", lines: "one\ntwo\tthree\r", control: "\u0001\u001f",
  wide: "é中😀", sep: "  ", empty: "",
}), JSON.stringify(text));
const want = [0, -0, 1, -1, 0.1 + 0.2, 1e21, 1e-7, 123456789012345680000, 5e-324, 1.7976931348623157e308,
  2 ** 53, -(2 ** 53) - 2, 1e-6, 999999999999999900000];
check("numbers keep every bit, -0 included", same(numbers, want), String(numbers));
check("nested arrays and objects keep their shape and key order", same(shapes,
  { "1": "index", a: [1, [2, [3, {}]], []], b: { c: { d: null } }, t: true, f: false, n: null, "": "blank" }),
  JSON.stringify(shapes));
check("an own __proto__ stays an own data property",
  Object.getPrototypeOf(proto) === Object.prototype && Object.prototype.hasOwnProperty.call(proto, "__proto__") &&
  proto.__proto__.x === 1 && proto.y === 2);

const [holey] = e.getGlobalsBatch([ix("holey")]);
check("undefined in an array stays undefined", holey.length === 3 && 1 in holey && holey[1] === undefined && holey[2] === 3);
const [notnumbers] = e.getGlobalsBatch([ix("notnumbers")]);
check("NaN and the infinities arrive", Number.isNaN(notnumbers.nan) && notnumbers.inf === Infinity && notnumbers.ninf === -Infinity);
const [lone] = e.getGlobalsBatch([ix("lone")]);
check("a lone surrogate arrives exact", lone.length === 3 && lone.charCodeAt(1) === 0xd800);

// A value JSON cannot carry first, last, and between ones it can.
const order = [ix("text"), ix("notnumbers"), ix("numbers"), ix("nested"), ix("shapes")];
const mixed = e.getGlobalsBatch(order);
check("a mixed batch arrives whole and in order", mixed.length === 5 && same(mixed[0], text) &&
  Number.isNaN(mixed[1].nan) && same(mixed[2], want) && same(mixed[4], shapes) &&
  mixed[3].deep.length === 1 && "v" in mixed[3].deep[0] && mixed[3].deep[0].v === undefined);
const last = e.getGlobalsBatch([ix("numbers"), ix("lone")]);
check("a value JSON cannot carry at the end of a batch", same(last[0], want) && last[1].charCodeAt(1) === 0xd800);

// Each read is fresh: nothing the host does to one shows in the next.
const first = e.getGlobalsBatch([ix("shapes")])[0];
first.a.push("host");
check("each read builds new objects", e.getGlobalsBatch([ix("shapes")])[0].a.length === 3);

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
