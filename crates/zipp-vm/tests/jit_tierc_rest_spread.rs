//! Tier C: object rest, spread Math calls and Object.values / entries in hot
//! functions over every kind of source (a string, an array, a proxy, a
//! getter that throws, an inherited key) and builtins replaced mid-run,
//! against Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"mix 731544573
caught 37"####;

const PROGRAM: &str = r####"// Tier C: object rest, spread Math calls and Object.values / entries in hot
// functions, over every kind of source, a getter that throws, a proxy, and a
// builtin replaced mid-run.
const out = [];

function restOf(o) {
  const { a, b, ...rest } = o;
  return [a, b, Object.keys(rest).join(","), Object.values(rest).join(",")].join("|");
}
function maxOf(xs) {
  return Math.max(...xs) + Math.min(...xs) * 2 + Math.hypot(...xs.slice(0, 3));
}
function enumOf(o) {
  let s = 0;
  for (const [k, v] of Object.entries(o)) s += k.length * 7 + (typeof v === "number" ? v : 1);
  for (const v of Object.values(o)) s += typeof v === "number" ? v : 3;
  return s;
}

let h = 0;
const mix = (v) => {
  h = (Math.imul(h ^ (typeof v === "number" ? v | 0 : String(v).length), 16777619) + 7) | 0;
};
for (let i = 0; i < 3000; i++) {
  const kind = i % 7;
  let src;
  if (kind === 0) src = { a: i, b: 2, c: 3, d: i & 7 };
  else if (kind === 1) src = { b: 1, z: 9, a: "x", [`k${i % 5}`]: i };
  else if (kind === 2) src = "abcdef".slice(0, 2 + (i % 4));
  else if (kind === 3) src = [i, i + 1, i + 2];
  else if (kind === 4) src = new Proxy({ a: 1, b: 2, q: i }, { get: (t, k) => (k === "q" ? t.q * 2 : t[k]) });
  else if (kind === 5) src = { a: 5, get g() { return i; }, b: 6 };
  else src = Object.create({ inherited: 1 }, { a: { value: 3, enumerable: true }, own: { value: 4, enumerable: true } });
  mix(restOf(src));
  const xs = [i % 9, -(i % 4), (i * 7) % 13, i & 1 ? -0 : 0, kind === 6 ? NaN : 1];
  mix(maxOf(xs));
  mix(enumOf(typeof src === "string" ? { s: src } : src));
  if (i === 2000) {
    const realMax = Math.max;
    Math.max = (...a) => realMax(...a) + 1000;
    Object.entries = ((real) => (o) => real(o).concat([["extra", 5]]))(Object.entries);
  }
}
out.push("mix " + h);

// A getter throwing inside the rest copy, caught outside.
let caught = 0;
for (let i = 0; i < 500; i++) {
  try {
    restOf({ a: 1, b: 2, get bad() { if (i % 50 === 0) throw new Error("x" + i); return i; } });
  } catch (e) {
    caught += e.message.length;
  }
}
out.push("caught " + caught);
console.log(out.join("\n"));
"####;

#[test]
fn matches_node() {
    // Read once per process, before the first VM: this file's only test.

    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
