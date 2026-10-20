//! Spread, Array.from, array destructuring and the Map/Set constructors
//! step a generator with the intrinsic `next` as a raw step, without the
//! `{value, done}` object `next` would make. A `yield*` forwarding a user
//! iterator's result object still reads that object; its getters run as
//! often as node runs them. A patched `next`, a throw mid-drain, a
//! destructuring that closes an unexhausted generator, and a second drain
//! of a finished one must all match node's output.

#[test]
fn generator_drains_match_the_iteration_protocol() {
    let src = r###""use strict";
const out = [];
const t = (f) => { try { return String(f()); } catch (e) { return e.constructor.name + ":" + e.message; } };
function* range(a, b) { for (let i = a; i < b; i++) yield i; return "ret"; }
out.push("1 " + [...range(0, 5)].join("") + " " + Array.from(range(2, 6), (x) => x * 10).join(",") + " " + Math.max(...range(0, 9)));
const m = new Map((function* () { yield ["a", 1]; yield ["b", 2]; })());
const s = new Set(range(3, 7));
out.push("2 " + [...m].join("|") + " " + [...s].join(""));
const [x, , y, ...rest] = range(10, 20);
out.push("3 " + x + y + " " + rest.join(""));
// destructuring that does not exhaust the generator closes it
let finals = 0;
function* closable() { try { yield 1; yield 2; yield 3; } finally { finals++; } }
const [p, q] = closable();
out.push("4 " + p + q + " finals=" + finals);
// yield* over a user iterator whose results have counting getters
let doneReads = 0, valueReads = 0;
const user = { [Symbol.iterator]() { let i = 0; return { next() { i++; return { get done() { doneReads++; return i > 3; }, get value() { valueReads++; return i * 7; } }; } }; } };
function* wrap() { yield* user; yield 99; }
out.push("5 " + [...wrap()].join(",") + " done=" + doneReads + " value=" + valueReads);
doneReads = valueReads = 0;
const [u1, u2] = wrap();
out.push("6 " + u1 + "," + u2 + " done=" + doneReads + " value=" + valueReads);
doneReads = valueReads = 0;
out.push("7 " + Array.from(wrap()).length + " " + new Set(wrap()).size + " done=" + doneReads + " value=" + valueReads);
// a patched next on one generator function's prototype
function* patched() { yield "real"; }
patched.prototype.next = function () { return this.n = (this.n || 0) + 1, this.n > 2 ? { done: true } : { value: "fake" + this.n, done: false }; };
out.push("8 " + [...patched()].join(",") + " " + Array.from(patched()).join(","));
// a throw mid-spread propagates, and the generator ends up completed
function* boom() { yield 1; throw new RangeError("bang"); }
const g = boom();
out.push("9 " + t(() => [...g]) + " " + JSON.stringify(g.next()));
// the generator's return value is not an element; a second drain is empty
const r = range(0, 3);
out.push("10 " + [...r].join("") + "|" + [...r].join("") + "|");
console.log(out.join("\n"));
"###;
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), r###"1 01234 20,30,40,50 8
2 a,1|b,2 3456
3 1012 13141516171819
4 12 finals=1
5 7,14,21,99 done=7 value=4
6 7,14 done=4 value=2
7 4 4 done=14 value=8
8 fake1,fake2 fake1,fake2
9 RangeError:bang {"done":true}
10 012||"###);
}
