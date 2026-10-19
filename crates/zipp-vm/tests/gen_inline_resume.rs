//! Generator steps run as frames of their consumer's dispatch loop
//! (`Vm::gen_enter_inline`): a `for-of` step, a `yield*` step and a
//! `g.next(v)` call. Each program's output is node's, and covers what the
//! frame must hand back: yields and returns, throws caught inside and
//! outside the generator, `finally` on break, return() and throw() into a
//! parked chain, raw `yield*` results, re-entrancy, deep nesting, mapped
//! `arguments` and direct-eval scopes.

fn run_ok(src: &str) -> String {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output.join("\n")
}

#[test]
fn for_of_steps_resume_in_the_consumers_loop() {
    let src = r###""use strict";
const out = [];
const log = (...a) => out.push(a.join(" "));

function* range(n) { for (let i = 0; i < n; i++) yield i; }
function* pass(it) { for (const x of it) yield x; }

// 1. return value of a generator ends the loop; nested levels
let s = 0;
for (const x of pass(pass(range(10)))) s += x;
log("1", s);

// 2. throw inside the generator, caught by the consumer
function* thrower(n) { for (let i = 0; i < n; i++) { if (i === 3) throw new Error("boom" + i); yield i; } }
try { for (const x of pass(thrower(10))) log("2v", x); } catch (e) { log("2c", e.message); }
const tg = thrower(10);
try { for (const x of tg) log("2w", x); } catch (e) { log("2d", e.message); }
log("2n", JSON.stringify(tg.next()));

// 3. throw caught inside the generator
function* catcher() { for (let i = 0; i < 4; i++) { try { if (i & 1) throw i; yield "ok" + i; } catch (e) { yield "caught" + e; } } }
for (const x of catcher()) log("3", x);

// 4. finally runs on break (return() through the close path) and on completion
let fin = 0;
function* withFin(n) { try { for (let i = 0; i < n; i++) yield i; } finally { fin++; log("4f", fin); } }
for (const x of withFin(5)) { if (x === 2) break; log("4v", x); }
for (const x of withFin(2)) log("4w", x);
for (const x of pass(withFin(3))) { if (x === 1) break; }
log("4", fin);

// 5. return inside try/finally in the generator
function* retFin() { try { yield 1; return 7; } finally { log("5f"); } }
const r5 = [];
for (const x of retFin()) r5.push(x);
log("5", r5.join(","));

// 6. re-entrancy: the generator steps itself
let self6;
function* reent() { yield 1; try { self6.next(); } catch (e) { log("6c", e.constructor.name); } yield 2; }
self6 = reent();
for (const x of self6) log("6v", x);

// 7. a generator that yields from a finally on break
function* yieldFin() { try { yield 1; yield 2; } finally { log("7f"); } }
for (const x of yieldFin()) { log("7v", x); break; }

// 8. next(v) mixed with for-of over the same generator
function* echo() { let got = 0; for (;;) { const v = yield got; if (v === undefined) return "end"; got = v; } }
const g8 = echo();
log("8a", JSON.stringify(g8.next()), JSON.stringify(g8.next(5)));
for (const x of g8) { log("8b", x); break; }
log("8c", JSON.stringify(g8.next(9)));

// 9. closures over generator locals across steps
function* closures() { for (let i = 0; i < 3; i++) yield () => i; }
log("9", [...closures()].map(f => f()).join(","));
const fs9 = []; for (const f of closures()) fs9.push(f); log("9b", fs9.map(f => f()).join(","));

// 10. yield* inside a consumer of an inline generator (falls back) and inside
function* deleg() { yield* range(3); return "d"; }
let s10 = []; for (const x of pass(deleg())) s10.push(x); log("10", s10.join(","));

// 11. deep generator nesting
function* deep(n) { if (n === 0) { yield 1; yield 2; return; } for (const x of deep(n - 1)) yield x + 1; }
let s11 = 0; for (const x of deep(3000)) s11 += x; log("11", s11);

// 12. exception thrown from the consumer's loop body while the generator is suspended
let fin12 = 0;
function* g12() { try { yield 1; yield 2; } finally { fin12++; } }
try { for (const x of g12()) { throw new Error("body" + x); } } catch (e) { log("12", e.message, fin12); }

// 13. generator argument handling and `this`
function* withThis(a, b = a * 2) { yield this === undefined; yield a + b; }
log("13", [...withThis(3)].join(","));
const o13 = { *m(k) { yield this.v * k; } , v: 5 };
for (const x of o13.m(4)) log("13b", x);

// 14. destructuring and spread consumers
const [a14, b14, ...c14] = pass(range(6));
log("14", a14, b14, c14.join(","));
log("14b", Math.max(...pass(range(9))));

// 15. many steps (counts)
let n15 = 0; for (const x of pass(range(100000))) n15 = (n15 + x) | 0; log("15", n15);

// 16. thrown non-Error values keep identity
const sym = { tag: "t" };
function* g16() { yield 1; throw sym; }
try { for (const x of pass(g16())) {} } catch (e) { log("16", e === sym); }

// 17. a generator step that calls a function that throws, caught in the generator
function bad() { throw new TypeError("bad"); }
function* g17() { for (let i = 0; i < 2; i++) { try { bad(); } catch (e) { yield e.message + i; } } }
for (const x of g17()) log("17", x);

console.log(out.join("\n"));
"###;
    let expected = r###"1 45
2v 0
2v 1
2v 2
2c boom3
2w 0
2w 1
2w 2
2d boom3
2n {"done":true}
3 ok0
3 caught1
3 ok2
3 caught3
4v 0
4v 1
4f 1
4w 0
4w 1
4f 2
4f 3
4 3
5f
5 1
6v 1
6c TypeError
6v 2
7v 1
7f
8a {"value":0,"done":false} {"value":5,"done":false}
8c {"done":true}
9 0,1,2
9b 0,1,2
10 0,1,2
11 6003
12 body1 1
13 true,9
13b 20
14 0 1 2,3,4,5
14b 8
15 704982704
16 true
17 bad0
17 bad1"###;
    assert_eq!(run_ok(src), expected);
}

#[test]
fn sloppy_generators_keep_arguments_and_eval_scopes() {
    let src = r###"// Sloppy-mode cases: mapped arguments, direct eval scopes.
const out = [];
const log = (...a) => out.push(a.join(" "));

function* range(n) { for (let i = 0; i < n; i++) yield i; }

function* mapped(a, b) { arguments[0] = 10; yield a; a = 20; yield arguments[0]; yield b; }
for (const x of mapped(1, 2)) log("1", x);

function* evaler(n) { for (let i = 0; i < n; i++) { eval("var q" + i + " = " + i); yield eval("q" + i) * 2; } }
for (const x of evaler(3)) log("2", x);

function* evalVar() { eval("var z = 5"); yield z; z++; yield z; }
for (const x of evalVar()) log("3", x);

function* sloppyThis() { yield typeof this; }
for (const x of sloppyThis()) log("4", x);

const named = function* self(n) { yield typeof self; if (n > 0) for (const x of self(n - 1)) yield x; };
for (const x of named(2)) log("5", x);

function* argsLen() { yield arguments.length; }
for (const x of argsLen(1, 2, 3)) log("6", x);

console.log(out.join("\n"));
"###;
    let expected = r###"1 10
1 20
1 2
2 0
2 2
2 4
3 5
3 6
4 object
5 function
5 function
5 function
6 3"###;
    assert_eq!(run_ok(src), expected);
}

#[test]
fn yield_star_steps_resume_in_the_consumers_loop() {
    let src = r###""use strict";
const out = [];
const log = (...a) => out.push(a.join(" "));

function* range(n) { for (let i = 0; i < n; i++) yield i; }
function* pass(it) { for (const x of it) yield x; }
function* deleg(it) { const r = yield* it; return r; }

// 1. yield* of a generator inside for-of, with the inner return value
function* withRet() { yield 1; yield 2; return "R"; }
function* outer1() { const r = yield* withRet(); yield "got " + r; }
for (const x of outer1()) log("1", x);
for (const x of pass(outer1())) log("1b", x);

// 2. yield* of user iterators and arrays (raw results), inside for-of
const userIt = { [Symbol.iterator]() { let i = 0; return { next() { return i < 3 ? { value: i++ * 10, done: false } : { value: "uend", done: true }; } }; } };
function* outer2() { const r = yield* userIt; yield "r=" + r; yield* [7, 8]; yield* "ab"; }
for (const x of outer2()) log("2", x);
for (const x of deleg(outer2())) log("2b", x);
log("2c", [...pass(deleg(outer2()))].join(","));

// 3. raw result objects with accessors and extra fields, forwarded through yield*
let reads = 0;
const accIt = { [Symbol.iterator]() { let i = 0; return { next() { i++; const o = {}; Object.defineProperty(o, "done", { get() { reads++; return i > 2; } }); Object.defineProperty(o, "value", { get() { reads++; return "v" + i; } }); return o; } }; } };
for (const x of deleg(accIt)) log("3", x);
log("3r", reads);
const g3 = deleg(accIt);
const s3 = g3.next();
log("3s", typeof s3, Object.keys(s3).length, s3.value, s3.done);

// 4. two-way: next(v) through nested yield*
function* acc(n) { let s = 0; for (let i = 0; i < n; i++) { const got = yield i; s += got === undefined ? 1 : got; } return s; }
function* two(n) { const a = yield* acc(n); const b = yield* acc(n >> 1); return a * 3 + b; }
const g4 = two(6);
let r4 = g4.next(), sent = 0, trace = [];
while (!r4.done) { trace.push(r4.value); sent = (sent + r4.value) & 7; r4 = g4.next(sent); }
log("4", trace.join(","), r4.value);
let f4 = []; for (const x of two(5)) f4.push(x); log("4b", f4.join(","));

// 5. recursive yield* tree
function makeTree(d, v) { return d === 0 ? null : { v, left: makeTree(d - 1, v * 2 + 1), right: makeTree(d - 1, v * 2 + 2) }; }
function* inorder(t) { if (t === null) return 0; const l = yield* inorder(t.left); yield t.v; const r = yield* inorder(t.right); return l + r + 1; }
const tree = makeTree(8, 1);
let h5 = 0, n5 = 0; const it5 = inorder(tree); let st = it5.next();
while (!st.done) { h5 = (Math.imul(h5, 31) + st.value) | 0; n5++; st = it5.next(); }
log("5", h5, n5, st.value);
let h5b = 0; for (const v of inorder(tree)) h5b = (Math.imul(h5b, 31) + v) | 0; log("5b", h5b);

// 6. inner throws through yield*; caught by an outer level, and uncaught
function* thrower() { yield 1; throw new Error("inner"); }
function* catcher() { try { yield* thrower(); } catch (e) { yield "caught " + e.message; } yield "after"; }
for (const x of catcher()) log("6", x);
for (const x of pass(catcher())) log("6b", x);
try { for (const x of deleg(deleg(thrower()))) log("6c", x); } catch (e) { log("6d", e.message); }

// 7. return() and throw() into a parked yield* chain
let fins = [];
function* finGen(tag) { try { yield tag + 1; yield tag + 2; } finally { fins.push(tag); } }
function* chainF() { try { yield* finGen("a"); } finally { fins.push("outer"); } }
for (const x of chainF()) { log("7", x); break; }
log("7f", fins.join(","));
const g7 = deleg(deleg(finGen("b")));
log("7n", JSON.stringify(g7.next()));
log("7r", JSON.stringify(g7.return(42)), fins.join(","));
const g7t = deleg(catcherT());
function* catcherT() { try { yield 1; } catch (e) { yield "t:" + e; } }
log("7t1", JSON.stringify(g7t.next()));
log("7t2", JSON.stringify(g7t.throw("X")));
log("7t3", JSON.stringify(g7t.next()));

// 8. a generator whose next was replaced, delegated to
function* plain() { yield 1; yield 2; }
const gp = plain();
gp.next = function () { return { value: "replaced", done: true }; };
function* d8() { const r = yield* gp; yield "r8 " + r; }
for (const x of d8()) log("8", x);

// 9. chain of mixed iterables
function* chain(...its) { for (const it of its) yield* it; }
log("9", [...chain(range(3), [9, 8], "xy", new Set([5]), pass(range(2)))].join(","));

// 10. re-entrancy through yield*
let g10;
function* inner10() { yield 1; try { g10.next(); } catch (e) { yield e.constructor.name; } }
function* outer10() { yield* inner10(); }
g10 = outer10();
for (const x of g10) log("10", x);

// 11. deep delegation chain
function* deepD(n) { if (n === 0) { yield "leaf"; return n; } return 1 + (yield* deepD(n - 1)); }
const g11 = deepD(2000); log("11", JSON.stringify(g11.next()), JSON.stringify(g11.next()));
let c11 = 0; for (const x of deepD(2500)) c11++; log("11b", c11);

// 12. yield* sends next(v) values into a user iterator
const sink = { [Symbol.iterator]() { const got = []; return { next(v) { got.push(v); return got.length > 3 ? { done: true, value: got.join("/") } : { done: false, value: got.length }; } }; } };
function* d12() { const r = yield* sink; return r; }
const g12 = d12(); let s12 = g12.next("a"), t12 = [];
while (!s12.done) { t12.push(s12.value); s12 = g12.next("v" + s12.value); }
log("12", t12.join(","), s12.value);

console.log(out.join("\n"));
"###;
    let expected = r###"1 1
1 2
1 got R
1b 1
1b 2
1b got R
2 0
2 10
2 20
2 r=uend
2 7
2 8
2 a
2 b
2b 0
2b 10
2b 20
2b r=uend
2b 7
2b 8
2b a
2b b
2c 0,10,20,r=uend,7,8,a,b
3 v1
3 v2
3r 8
3s object 0 v1 false
4 0,1,2,3,4,5,0,1,2 66
4b 0,1,2,3,4,0,1
5 -1810849814 255 255
5b -1810849814
6 1
6 caught inner
6 after
6b 1
6b caught inner
6b after
6c 1
6d inner
7 a1
7f a,outer
7n {"value":"b1","done":false}
7r {"value":42,"done":true} a,outer,b
7t1 {"value":1,"done":false}
7t2 {"value":"t:X","done":false}
7t3 {"done":true}
8 r8 replaced
9 0,1,2,9,8,x,y,5,0,1
10 1
10 TypeError
11 {"value":"leaf","done":false} {"value":2000,"done":true}
11b 1
12 1,2,3 /v1/v2/v3"###;
    assert_eq!(run_ok(src), expected);
}

#[test]
fn next_calls_resume_in_the_callers_loop() {
    let src = r###""use strict";
const out = [];
const log = (...a) => out.push(a.join(" "));

function* range(n) { for (let i = 0; i < n; i++) yield i; }
function* take(it, n) { if (n <= 0) return; for (const x of it) { yield x; if (--n <= 0) return; } }
function* mapGen(it, f) { for (const x of it) yield f(x); }

// 1. explicit next() calls, result objects are fresh and well-formed
const g1 = range(3);
const rs = [g1.next(), g1.next(), g1.next(), g1.next(), g1.next()];
log("1", JSON.stringify(rs), rs[0] !== rs[1], Object.keys(rs[0]).join(","));

// 2. next(v) delivers v
function* echo() { let v = yield "start"; while (true) { v = yield "got " + v; } }
const g2 = echo();
log("2", g2.next("ignored").value, g2.next(1).value, g2.next({}).value, g2.next().value);

// 3. scheduler over suspended pipelines
const tasks = [];
for (let t = 0; t < 6; t++) tasks.push(take(mapGen(range(100), x => x ^ t), 5 + t));
let live = tasks.length, h = 0, ticks = 0;
while (live > 0) {
  for (let t = 0; t < tasks.length; t++) {
    const task = tasks[t];
    if (task === null) continue;
    const step = task.next();
    if (step.done) { tasks[t] = null; live--; continue; }
    ticks++; h = (h + step.value * (t + 1)) | 0;
  }
}
log("3", h, ticks);

// 4. throw from a next() step, then the generator is done
function* bad() { yield 1; throw new RangeError("r"); }
const g4 = bad();
log("4a", JSON.stringify(g4.next()));
try { g4.next(); } catch (e) { log("4b", e.name, e.message); }
log("4c", JSON.stringify(g4.next()));

// 5. return value through next()
function* rv() { yield 1; return { k: 2 }; }
const g5 = rv(); g5.next(); const d5 = g5.next();
log("5", d5.done, JSON.stringify(d5.value), JSON.stringify(g5.next()));

// 6. next() of a yield* over a user iterator returns the inner result object verbatim
const inner = { i: 0, [Symbol.iterator]() { return this; }, next() { this.i++; return { value: this.i, done: this.i > 2, extra: "x" + this.i }; } };
function* d6() { yield* inner; }
const g6 = d6();
const s6a = g6.next(), s6b = g6.next(), s6c = g6.next();
log("6", JSON.stringify(s6a), JSON.stringify(s6b), JSON.stringify(s6c));

// 7. next() called on a generator from inside another generator's body
function* driver(g) { let r; while (!(r = g.next()).done) yield r.value * 10; }
log("7", [...driver(range(4))].join(","));

// 8. re-entrant next() inside its own step
let g8;
function* self8() { try { g8.next(); } catch (e) { yield e.constructor.name; } }
g8 = self8();
log("8", JSON.stringify(g8.next()), JSON.stringify(g8.next()));

// 9. next with extra args, and next on a non-pristine generator
function* two() { const a = yield 1; yield a; }
const g9 = two(); g9.next(); log("9", g9.next(5, 6, 7).value);
const g9b = two(); g9b.next = function () { return "patched"; }; log("9b", g9b.next());

// 10. generator finally runs on return() after next()-driven steps
let f10 = 0;
function* fin() { try { yield 1; yield 2; } finally { f10++; } }
const g10 = fin(); g10.next(); log("10", JSON.stringify(g10.return(9)), f10, JSON.stringify(g10.next()));

console.log(out.join("\n"));
"###;
    let expected = r###"1 [{"value":0,"done":false},{"value":1,"done":false},{"value":2,"done":false},{"done":true},{"done":true}] true value,done
2 start got 1 got [object Object] got undefined
3 739 45
4a {"value":1,"done":false}
4b RangeError r
4c {"done":true}
5 true {"k":2} {"done":true}
6 {"value":1,"done":false,"extra":"x1"} {"value":2,"done":false,"extra":"x2"} {"done":true}
7 0,10,20,30
8 {"value":"TypeError","done":false} {"done":true}
9 5
9b patched
10 {"value":9,"done":true} 1 {"done":true}"###;
    assert_eq!(run_ok(src), expected);
}
