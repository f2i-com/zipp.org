//! The hole-aware array callback path (holey arrays, array-likes) and
//! `flatMap` run a compiled callback over one reused native window, as the
//! dense path does: holes skipped, mutation mid-iteration observed, throws and
//! type changes in the callback, `this` binding and `reduce`'s accumulator as
//! the spec has them. Node's output.

const EXPECTED: &str = r####"99,145440,63,true,301,0,true,true,7,,144000,1792
199,289440,63,true,601,0,true,true,7,,288000,3584
299,433440,63,true,901,0,true,true,7,,432000,4096
399,577440,63,true,1201,0,true,true,7,,576000,7168
mutDelete=15
mutPush=16:27
mutCut=23:,2,3,4,5,
throwMap=threw cb13
throwReduce=threw r17
bail=23:,4,7,0,13,
thisStrict=|number|number|number|number||number|number|number|number|||
thisSloppy=|object|object|object|object||object|object|object|object|||
thisObj=4:6,7,8,9
reduceEmpty=threw Reduce of empty array with no initial value
likeMap=8:4,,10,,,19
likeReduce=92
fm=1635000
fmNested=3:1,2,2,4,3,6
fmMutate=6:1,0,2,1,3,2
fmThrow=threw fm3
fmThis=2:11,12"####;

const PROGRAM: &str = r####"// Compiled callbacks on the hole-aware array path and flatMap, against Node.
"use strict";
function holey(n, k) { const a = []; for (let i = 0; i < n; i++) a.push(i * k); for (let i = 0; i < n; i += 5) delete a[i]; a.length = n + 3; return a; }
let out = [];
function run(label, f) { let r; try { r = f(); } catch (e) { r = "threw " + e.message; } out.push(label + "=" + (Array.isArray(r) ? r.length + ":" + r.slice(0, 6).join(",") : String(r))); }
const add = (x) => (x * 3 + 1) | 0;
const odd = (x) => (x & 1) === 1;
for (let rep = 0; rep < 400; rep++) {
  const a = holey(60, rep + 1);
  let s = 0;
  a.forEach((v, i) => { s = (s + v + i) | 0; });
  const m = a.map(add);
  const f = a.filter(odd);
  const so = a.some((v) => v > 50 * (rep + 1));
  const ev = a.every((v) => v >= 0);
  const fi = a.findIndex((v) => v === 7 * (rep + 1));
  const fl = a.findLast((v) => v < 30);
  const r1 = a.reduce((acc, v) => (acc + v) | 0, 0);
  const r2 = a.reduceRight((acc, v) => (acc ^ v) | 0);
  if (rep % 100 === 99) out.push([rep, s, m.length, 1 in m, m[1], f.length, so, ev, fi, fl, r1, r2].join(","));
}
// Mutation mid-iteration: deletes ahead, pushes, length cuts.
run("mutDelete", () => { const a = holey(20, 1); let seen = 0; a.forEach((v, i) => { seen++; if (i === 3) delete a[8]; }); return seen; });
run("mutPush", () => { const a = holey(20, 1); let seen = 0; a.forEach((v) => { seen++; if (seen < 5) a.push(99); }); return seen + ":" + a.length; });
run("mutCut", () => { const a = holey(20, 1); return a.map((v, i) => { if (i === 6) a.length = 9; return v + 1; }); });
// Throws and type changes in the compiled callback.
const thrower = (v, i) => { if (i === 13) throw new Error("cb" + i); return v * 2; };
run("throwMap", () => holey(20, 1).map(thrower));
run("throwReduce", () => holey(20, 1).reduce((acc, v, i) => { if (i === 17) throw new Error("r" + i); return acc + v; }, 0));
run("bail", () => { const a = holey(20, 1); a[3] = "s"; a[7] = 2.5; return a.map(add); });
// this binding: strict, sloppy via a Function, an object thisArg.
run("thisStrict", () => holey(10, 1).map(function () { return typeof this; }, 5).join("|"));
const sloppy = Function("v", "return this === undefined ? 'u' : typeof this;");
run("thisSloppy", () => holey(10, 1).map(sloppy).join("|"));
run("thisObj", () => holey(10, 1).filter(function (v) { return v > this.min; }, { min: 4 }));
run("reduceEmpty", () => [, , ,].reduce((a, b) => a + b));
// Array-likes.
const like = { length: 8, 0: 1, 2: 3, 5: 6, 7: 8 };
run("likeMap", () => Array.prototype.map.call(like, add));
run("likeReduce", () => Array.prototype.reduce.call(like, (a, v, i) => a + v * i, 0));
// flatMap with a compiled mapper.
let fm = 0;
for (let rep = 0; rep < 400; rep++) {
  const src = holey(40, rep + 1);
  const r = src.flatMap((x, i) => (x & 1 ? [x, i] : (x % 4 === 0 ? [] : x * 2)));
  fm = (fm + r.length * 7 + (r[r.length - 1] | 0)) | 0;
}
out.push("fm=" + fm);
run("fmNested", () => [1, 2, 3].flatMap((x) => [[x, x * 2]]));
run("fmMutate", () => { const a = [1, 2, 3, 4]; return a.flatMap((x, i) => { if (i === 1) a.length = 3; return [x, i]; }); });
run("fmThrow", () => [1, 2, 3, 4].flatMap((x) => { if (x === 3) throw new Error("fm3"); return x; }));
run("fmThis", () => [1, 2].flatMap(function (x) { return [this.k + x]; }, { k: 10 }));
console.log(out.join("\n"));
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
