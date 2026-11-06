//! Array builtins and `in` on their compiled lanes, against Node: `shift`,
//! `unshift`, `splice` and `reverse` through `jit_array_mutate` (holes, a
//! prototype index, coercing `splice` arguments, a subclass species, frozen
//! arrays, a non-writable length, an own shadow, a method replaced
//! mid-run); `indexOf`/`lastIndexOf`/`includes` over mixed kinds, -0, NaN,
//! holes and BigInt; `Object.keys`/`values` of arrays (holes, a trailing
//! length, named properties, a non-enumerable index, frozen, a match
//! result, `arguments`, a long array, a getter index) and `for-in`; `in` on
//! dense and holey arrays with negative, double, accessor and prototype
//! keys; an array only ever written by index holding young objects across
//! collections; `join` over primitives, holes and objects, `fill` with
//! coercing positions, `slice` edge positions and holes, and `concat` with
//! an `@@isConcatSpreadable` flag on an array and on the prototype. In the
//! interpreter, by default and with every function compiled at once.

const SOURCE: &str = r##""use strict";
// Array builtins and `in` on their compiled lanes, hot enough to compile.
const out = [];
function hot(n, f) { let s = 0; for (let i = 0; i < n; i++) s = (s * 31 + f(i)) | 0; return s; }
// 1. shift/unshift/splice/reverse on plain arrays
out.push("Q " + hot(3000, (i) => {
  const q = [1, 2, 3, i];
  const a = q.shift(); const b = q.unshift(i, i + 1); const r = q.reverse();
  const sp = q.splice(1, 2, "x", "y", "z"); const sp2 = q.splice(-2); const sp3 = q.splice(0, 0);
  return a + b + (r === q ? 1 : 0) + sp.length * 10 + sp2.length * 100 + sp3.length + q.length * 1000 + String(q).length;
}));
// 2. holes: a leading hole's shift, splice keeping holes, a prototype index
out.push("H " + hot(3000, (i) => {
  const h = [, 1, , 3]; const s0 = h.shift(); const n = h.length; const k = 1 in h;
  const g = [1, , 3, , 5]; const rm = g.splice(1, 2); return (s0 === undefined ? 1 : 0) + n + (k ? 1 : 0) + (1 in rm ? 10 : 20) + g.length;
}));
Array.prototype[0] = "P";
out.push("P " + hot(3000, (i) => [, 2].shift().length + [, 2, 3].splice(0, 1)[0].length + (0 in [, 1] ? 1 : 0)));
delete Array.prototype[0];
// 3. coercing splice arguments, species, frozen, non-writable length, an own shadow
let calls = 0; const vo = { valueOf() { calls++; return 1; } };
out.push("C " + hot(3000, (i) => [1, 2, 3].splice(vo, vo).length) + " " + calls);
class MyA extends Array {}
out.push("S " + hot(3000, (i) => { const m = MyA.from([1, 2, 3]); const r = m.splice(0, 1); return (r instanceof MyA ? 1 : 0) + m.length; }));
const fr = Object.freeze([1, 2]);
out.push("F " + hot(3000, (i) => { try { fr.shift(); return 0; } catch (e) { return e instanceof TypeError ? 1 : 2; } }));
const nw = [1, 2, 3]; Object.defineProperty(nw, "length", { writable: false });
out.push("N " + hot(3000, (i) => { try { nw.unshift(0); return 0; } catch (e) { return e instanceof TypeError ? 1 : 2; } }) + " " + nw.join());
const own = [1, 2, 3]; own.shift = function () { return 42; };
out.push("O " + hot(3000, (i) => own.shift()));
// 4. a method replaced on the prototype mid-run
const saved = Array.prototype.reverse; let rv = 0;
out.push("R " + hot(3000, (i) => { if (i === 1500) Array.prototype.reverse = function () { rv++; return this; }; return [1, 2, 3].reverse()[0]; }) + " " + rv);
Array.prototype.reverse = saved;
// 5. indexOf/lastIndexOf/includes over mixed kinds, -0, NaN, holes, BigInt
const mix = [1, 2.5, "a", -0, NaN, null, undefined, true, { o: 1 }, 2, "a", 1n, 3];
const objv = mix[8];
out.push("I " + hot(3000, (i) => mix.indexOf(2) + mix.indexOf(2.5) * 3 + mix.indexOf("a") * 7 + mix.lastIndexOf("a") * 11 + mix.indexOf(0) * 13 + mix.indexOf(NaN) * 17 + mix.indexOf(null) * 19 + mix.indexOf(undefined) * 23 + mix.indexOf(true) * 29 + mix.indexOf(objv) * 31 + mix.indexOf({}) * 37 + mix.indexOf(1n) * 41 + mix.lastIndexOf(1) * 43 + mix.indexOf(3, -2) * 47 + mix.lastIndexOf(2, 5) * 53 + (mix.includes(NaN) ? 59 : 0)));
const dbl = [1.5, 2, 3.0, 4]; out.push("D " + dbl.indexOf(3) + " " + dbl.indexOf(2.0) + " " + [0].indexOf(-0) + " " + [-0].lastIndexOf(0) + " " + [, 1].indexOf(undefined) + " " + [, 1].lastIndexOf(undefined));
const sl = []; for (let i = 0; i < 300; i++) sl.push(i); delete sl[150];
out.push("L " + hot(300, (i) => { const s = sl.slice(i % 200, 260); return s.length + (s.indexOf(149) >= 0 ? 1 : 0) + ((150 - (i % 200)) in s ? 1 : 0); }));
// 6. Object.keys / values of arrays: holes, trailing length, named props, overlays, frozen, match results, arguments, long
const a6 = [1, 2, , 4]; a6.length = 7;
const b6 = [5, 6, 7]; b6.x = 1;
const c6 = [1, 2, 3]; Object.defineProperty(c6, 1, { value: 9, enumerable: false });
const d6 = Object.freeze([1, 2, 3]);
const m6 = /(a)(b)?/.exec("xab");
function args6() { return Object.keys(arguments).join(","); }
const big = []; for (let i = 0; i < 3000; i++) big.push(i); delete big[5];
out.push("K " + hot(300, (i) => Object.keys(a6).length + Object.values(a6).length * 3 + Object.keys(b6).length * 5 + Object.values(c6).length * 7 + Object.keys(d6).length * 11 + Object.keys(m6).length * 13 + args6(1, 2, 3).length * 17 + Object.keys(big).length));
out.push("K2 " + JSON.stringify([Object.keys(a6), Object.values(a6), Object.keys(b6), Object.keys(c6), Object.keys(m6)]));
const kb = Object.keys(big); out.push("K3 " + kb[4] + " " + kb[5] + " " + kb[2998] + " " + typeof kb[0] + " " + (Object.keys([7, 8])[1] === Object.keys([9, 9])[1]));
const h6 = [1, 2, 3]; Object.defineProperty(h6, 0, { get() { return 42; }, enumerable: true });
out.push("K4 " + JSON.stringify(Object.values(h6)) + " " + JSON.stringify(Object.keys(Object.assign([], { 3: 1 }))));
const fi = []; for (const k in a6) fi.push(k); out.push("K5 " + fi.join(","));
// 7. `in` over dense and holey arrays, negative and double keys, an accessor index, a prototype index
const dense = []; for (let i = 0; i < 64; i++) dense.push(i);
const holey = dense.slice(); for (let i = 0; i < 64; i += 7) delete holey[i];
out.push("IN " + hot(3000, (i) => { let n = 0; for (let k = -2; k < 70; k++) { if (k in dense) n++; if (k in holey) n += 2; } return n + ((1.5 in dense) ? 100 : 0) + ((3.0 in holey) ? 1000 : 0); }));
const acc = [1, , 3]; Object.defineProperty(acc, 1, { get() { return 2; } });
Array.prototype[7] = 1;
out.push("IN2 " + hot(3000, (i) => (1 in acc ? 1 : 0) + (7 in dense ? 2 : 0) + (7 in holey ? 4 : 0) + (70 in holey ? 8 : 0)));
delete Array.prototype[7];
// 8. an array only ever written by index (stores settle its mirror), holding young objects across collections
const ring = new Array(512).fill(null);
function put(i) { ring[i & 511] = { v: i, w: [i, -i] }; }
for (let i = 0; i < 200000; i++) put(i);
let rs = 0; for (let i = 0; i < 512; i++) rs = (rs + ring[i].v * 3 + ring[i].w[0] + ring[i].w[1] * 2) | 0;
out.push("W " + rs);
// 9. join over primitives, holes, -0, BigInt, separators; objects with toString, a Symbol; fill and concat
let ts = 0; const tobj = { toString() { ts++; return "T"; } };
const sym = Symbol("s");
out.push("J " + hot(3000, (i) => {
  const a = [1, -0, "x", null, undefined, true, 2.5, 3n, , i];
  const r = a.join() + a.join("-") + a.join("") + [tobj, 1].join("+") + [[1, 2], [3]].join(";");
  let e = 0; try { [1, sym].join(); } catch (err) { e = err instanceof TypeError ? 1 : 2; }
  return r.length * 3 + e + r.charCodeAt(i % r.length);
}) + " " + ts);
let fv = 0; const fvo = { valueOf() { fv++; return 2; } };
out.push("FL " + hot(3000, (i) => {
  const f = new Array(8).fill(i); f.fill(-1, 2, 4); f.fill(7, -2); f.fill(9, fvo, 3); const g = [1, , 3].fill(0, 1, 2);
  let fe = 0; try { Object.freeze([1, 2]).fill(0); } catch (err) { fe = err instanceof TypeError ? 1 : 2; }
  return f.join("").length * 11 + f[3] + f[7] + f[2] + (1 in g ? 100 : 0) + fe;
}) + " " + fv);
const s10 = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
out.push("SL " + hot(3000, (i) => {
  const parts = [s10.slice(), s10.slice(3), s10.slice(-3), s10.slice(2, -2), s10.slice(-0, 4), s10.slice(NaN, 5), s10.slice(1, Infinity),
    s10.slice(-Infinity, 2), s10.slice(undefined, undefined), s10.slice(8, 2), s10.slice(2.9, 6.1), s10.slice(i % 12, (i * 7) % 13)];
  let t = 0; for (const p of parts) t = (t * 7 + p.length * 13 + (p.length ? p[0] + p[p.length - 1] : 0)) | 0;
  return t;
}));
const sv = []; sv.length = 5; sv[1] = 1; sv[3] = 3; Object.defineProperty(sv, "x", { value: 1 });
out.push("SL2 " + JSON.stringify([sv.slice(0, 4), [, 1, 2].slice(0, 2), Object.keys([, 1, 2].slice(0, 2))]));
out.push("SP " + hot(3000, (i) => {
  const a = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
  const r = [a.splice(2, 2, "a", "b"), a.splice(NaN, 1), a.splice(Infinity, 3, "z"), a.splice(-Infinity, 0, "s"), a.splice(4, Infinity),
    a.splice(undefined, undefined), a.splice(1, -5, "n"), a.splice(-1, 1, "p", "q", "r"), a.splice(20, 2), a.splice(2.7, 1.9, i)];
  let t = a.length * 1000; for (const x of r) t = (t * 31 + x.length * 7 + (x.length ? String(x[0]).charCodeAt(0) : 0)) | 0;
  return (t + String(a).length) | 0;
}));
const cs = []; cs[Symbol.isConcatSpreadable] = false;
out.push("CC " + hot(3000, (i) => [1, 2].concat([3], i, [[4]], cs).length) + " " + [1].concat(cs).length);
Array.prototype[Symbol.isConcatSpreadable] = false;
out.push("CC2 " + [1, 2].concat([3, 4]).length);
delete Array.prototype[Symbol.isConcatSpreadable];
out.push("CC3 " + [1, 2].concat([3, 4]).length);
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "Q -880234464\\nH -1766349952\\nP -59063680\\nC 1411967872 6000\\nS -59063680\\nF 1411967872\\nN 1411967872 1,2,3\\nO -826891520\\nR 481408768 1500\\nI 1272956672\\nD 2 1 0 0 -1 -1\\nL -1782056950\\nK -1339365184\\nK2 [[\"0\",\"1\",\"3\"],[1,2,4],[\"0\",\"1\",\"2\",\"x\"],[\"0\",\"2\"],[\"0\",\"1\",\"2\",\"index\",\"input\",\"groups\"]]\\nK3 4 6 2999 string true\\nK4 [42,2,3] [\"3\"]\\nK5 0,1,3\\nIN 1263937024\\nIN2 1293840512\\nW 204537344\\nJ -116382733 3000\\nFL -139834880 3000\\nSL 1233663033\\nSL2 [[null,1,null,3],[null,1],[\"1\"]]\\nSP 1825349920\\nCC -118127360 2\\nCC2 2\\nCC3 4";

#[test]
fn array_builtin_lanes_worker() {
    if std::env::var_os("ZIPP_ARRAY_BUILTIN_LANES_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn array_builtin_lanes_match_node() {
    if std::env::var_os("ZIPP_ARRAY_BUILTIN_LANES_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [
        ("default", None),
        ("interpreter", Some("ZIPP_NOJIT")),
        ("compile-at-once", Some("ZIPP_JIT_THRESHOLD")),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["array_builtin_lanes_worker", "--exact", "--nocapture"])
            .env("ZIPP_ARRAY_BUILTIN_LANES_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD");
        if let Some(v) = var {
            command.env(v, "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}