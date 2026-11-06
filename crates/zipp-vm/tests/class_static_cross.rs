//! A compiled `C.m(x)` on a class value and `v instanceof C` against a class
//! with its own static `[Symbol.hasInstance]` call the method native to native
//! (`jit_class_static_callee` / `jit_own_has_instance`); before, both went
//! through `call_value` in a nested interpreter loop. Every way the resolved
//! method can change mid-run: reassigned, deleted, replaced by a getter,
//! inherited through `extends` then shadowed, the parent's replaced, a class
//! re-prototyped with `Object.setPrototypeOf`; `this`, `super` and an
//! arrow static field; generator, async, throwing and non-function statics;
//! a `[Symbol.hasInstance]` replaced, deleted, made a getter, inherited,
//! throwing, answering non-booleans and given primitives; a static method's
//! `this.#m()` on a subclass, which has no brand. The expected lines
//! are Node's (v24), with the lanes, without them, in the interpreter and
//! with every function compiled at once.

const SOURCE: &str = r##"
"use strict";
// Static method calls and own static [Symbol.hasInstance] from hot compiled loops: every way the
// resolved method can change or stop being a plain data method mid-run.
const out = [];
function loop(n, f) { let s = 0; for (let i = 0; i < n; i++) s = (s + f(i)) | 0; return s; }

// 1. own static, reassigned and deleted mid-loop
class A { static m(x) { return x + 1; } static get k() { return 7; } }
out.push("A " + loop(3000, (i) => {
  if (i === 1000) A.m = function (x) { return x * 2; };
  if (i === 2000) { delete A.m; A.m = (x) => x - 1; }
  return A.m(i);
}));
// 2. a getter defined over the method later
class B { static m(x) { return x & 3; } }
out.push("B " + loop(3000, (i) => {
  if (i === 1500) Object.defineProperty(B, "m", { get() { return (x) => x | 8; }, configurable: true });
  return B.m(i);
}));
// 3. inherited static through extends, then shadowed on the child, then the parent's replaced
class P { static m(x) { return x + 100; } }
class C extends P {}
out.push("C " + loop(3000, (i) => {
  if (i === 1000) C.m = function (x) { return x + 200; };
  if (i === 2000) { delete C.m; P.m = function (x) { return x + 300; }; }
  return C.m(i);
}));
// 4. setPrototypeOf on a class
class D { }
class E2 { static m(x) { return x ^ 5; } }
out.push("D " + loop(3000, (i) => {
  if (i === 1500) Object.setPrototypeOf(D, E2);
  try { return D.m(i); } catch (e) { return -1; }
}));
// 5. this inside a static method, an arrow static field, super in a static method
class F { static base = 10; static m(x) { return this.base + x; } static h = (x) => this.base * x; }
class G extends F { static base = 20; static m(x) { return super.m(x) + 1000; } }
out.push("F " + loop(2000, (i) => F.m(i) + F.h(i & 7) + G.m(i) + G.h(1)));
// 6. generator and async statics, a static that throws, a non-function static
class H { static *gen(n) { yield n; yield n + 1; } static async am(x) { return x; } static t(x) { if (x === 777) throw new Error("t" + x); return x; } static v = 5; }
let thrown = 0, gsum = 0;
for (let i = 0; i < 2000; i++) {
  const it = H.gen(i); gsum += it.next().value + it.next().value;
  H.am(i);
  try { H.t(i); } catch (e) { thrown++; }
  try { H.v(i); } catch (e) { thrown += 1000; }
}
out.push("H " + gsum + " " + thrown);
// 7. own static [Symbol.hasInstance]: replaced, deleted (falls back to the ordinary test), throwing,
// non-boolean results, this, primitive left operands, an inherited one, a getter one
class Even { static [Symbol.hasInstance](o) { return (o & 1) === 0 ? "yes" : 0; } }
class Base { }
class Kid extends Base { }
class Inh extends Even { }
const objs = [new Base(), new Kid(), {}, 4, 5, null, undefined, "s"];
let hits = [0, 0, 0, 0, 0, 0];
for (let i = 0; i < 4000; i++) {
  const o = objs[i & 7];
  if (i === 1000) Even[Symbol.hasInstance] = function (v) { return this === Even && typeof v === "number"; };
  if (i === 2000) delete Even[Symbol.hasInstance];
  if (i === 3000) Object.defineProperty(Even, Symbol.hasInstance, { get() { return (v) => v === 5; }, configurable: true });
  try { if (o instanceof Even) hits[0]++; } catch (e) { hits[1]++; }
  if (o instanceof Kid) hits[2]++;
  if (i & 1 ? o instanceof Inh : 0) hits[3]++;
}
class Thrower { static [Symbol.hasInstance](o) { if (o === 5) throw new Error("no5"); return true; } }
for (let i = 0; i < 2000; i++) { try { if (objs[i & 7] instanceof Thrower) hits[4]++; } catch (e) { hits[5]++; } }
out.push("I " + hits.join(","));
// 8. a static method using static private members through `this`, called on the class and on a
// subclass (no brand: TypeError), and a private `in` test through a static method
class SP { static #pm() { return 1; } static #pf = 2; static f() { return this.#pm() + this.#pf; } static has(o) { return #pf in o; } }
class SQ extends SP {}
// the private method declared after the static method that calls it
class SR { static f() { return this.#g(); } static #g() { return 40; } }
class SS extends SR {}
const sp = [0, 0, 0];
for (let i = 0; i < 3000; i++) {
  try { sp[0] += (i & 1 ? SP : SQ).f(); } catch (e) { sp[1] += e instanceof TypeError ? 1 : 1000; }
  if (SP.has(i & 1 ? SP : SQ)) sp[2]++;
  try { sp[0] += (i & 1 ? SR : SS).f(); } catch (e) { sp[1] += e instanceof TypeError ? 1 : 1000; }
}
out.push("J " + sp.join(","));
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "A 5998000\nB 3382516\nC 5098500\nD 3372734\nF 6148000\nH 4000000 2000001\nI 1250,0,500,500,1750,250\nJ 64500,3000,1500";

#[test]
fn class_static_cross_worker() {
    if std::env::var_os("ZIPP_CLASS_STATIC_CROSS_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn class_static_cross_matches_node() {
    if std::env::var_os("ZIPP_CLASS_STATIC_CROSS_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [
        ("default", None),
        ("lanes-off", Some("ZIPP_NO_STATIC_CROSS")),
        ("interpreter", Some("ZIPP_NOJIT")),
        ("compile-at-once", Some("ZIPP_JIT_THRESHOLD")),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["class_static_cross_worker", "--exact", "--nocapture"])
            .env("ZIPP_CLASS_STATIC_CROSS_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_STATIC_CROSS")
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