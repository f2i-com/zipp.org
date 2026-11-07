//! A compiled private METHOD call: a `this.#m` read hits the site's way-0
//! private method (`ic_private_method_way0`), and `recv.#m(...)` resolves
//! through that way, filling it on a miss (`jit_private_method_callee`), in
//! the Tier C cross-call lane and in the region call helper, where it used to
//! be looked up without its site and called through `call_value` each time.
//! Brandless receivers (a subclass's static call, a plain object,
//! `Object.create(instance)`), private getters and arrow fields called as
//! methods, `arguments` and arrows inside a private method, deep recursion
//! through one, classes evaluated per call (fresh brands each time, a
//! method of one rejecting another's instance) and a nested class shadowing
//! the outer name. The expected lines are Node's (v24), with the lanes,
//! without them, in the interpreter and with every function compiled at once.

const SOURCE: &str = r##"
"use strict";
// Private method calls (`this.#m()` and `o.#m()`) from hot compiled code: every resolution shape.
const out = [];
class A {
  #n = 0;
  #inc(k) { this.#n += k; return this.#n; }
  #args() { return arguments.length; }
  #arrowy(k) { const f = () => this.#n + k; return f(); }
  get #g() { return () => 7; }
  #f = (x) => x * 2;
  static #s(x) { return x + 1000; }
  run(k) { return this.#inc(k) + this.#args(1, 2) + this.#arrowy(k); }
  other(o) { return o.#inc(1); }
  viaGetter() { return this.#g(); }
  viaField(x) { return this.#f(x); }
  static stat(x) { return this.#s(x); }
  deep(n) { return n === 0 ? 0 : 1 + this.#deep2(n - 1); }
  #deep2(n) { return this.deep(n); }
}
class B extends A {}
const a = new A(), b = new B();
let s = 0, errs = [0, 0, 0, 0];
for (let i = 0; i < 4000; i++) {
  s = (s + a.run(i & 3) + b.run(1) + a.viaGetter() + a.viaField(i & 7)) | 0;
  s = (s + A.stat(i & 15)) | 0;
  try { s += B.stat(1); } catch (e) { errs[0] += e instanceof TypeError ? 1 : 1000; }   // no brand on the subclass
  try { s += a.other(i & 1 ? b : { }); } catch (e) { errs[1] += e instanceof TypeError ? 1 : 1000; }
  try { s += a.other(Object.create(a)); } catch (e) { errs[2] += e instanceof TypeError ? 1 : 1000; }
  if ((i & 255) === 0) s = (s + a.deep(300)) | 0;
}
out.push("A " + s + " " + errs.join(","));
// A class evaluated per call: fresh private names (brands) each time; a method of one evaluation
// must reject an instance of another.
function makeK(base) {
  return class K {
    #v = base;
    #get() { return this.#v; }
    read() { return this.#get(); }
    readOf(o) { return o.#get(); }
  };
}
let k = 0, kErr = 0;
const ks = [];
for (let r = 0; r < 40; r++) {
  const K = makeK(r);
  const x = new K(), y = new K();
  ks.push(x);
  for (let i = 0; i < 300; i++) {
    k = (k + x.read() + y.readOf(x)) | 0;
    if (r > 0 && i === 7) { try { k += x.readOf(ks[r - 1]); } catch (e) { kErr += e instanceof TypeError ? 1 : 1000; } }
  }
}
out.push("K " + k + " " + kErr);
// A nested class whose method shadows the outer private name.
class Outer {
  #m() { return "outer"; }
  callOwn() { return this.#m(); }
  inner() {
    const self = this;
    return class Inner {
      #m() { return "inner"; }
      callInner() { return this.#m(); }
      callOuter() { try { return self.#m(); } catch (e) { return "err"; } }
      callOuterOn(o) { try { return o.#m(); } catch (e) { return "TypeError"; } }
    };
  }
}
const ou = new Outer();
const Inner = ou.inner();
const inn = new Inner();
let tags = "";
for (let i = 0; i < 3000; i++) {
  const t = ou.callOwn() + inn.callInner() + inn.callOuter() + inn.callOuterOn(i & 1 ? ou : inn);
  if (i % 1000 === 0) tags += t + ";";
}
out.push("N " + tags);
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "A 58115800 4000,2000,4000,0\nK 468000 39\nN outerinnererrinner;outerinnererrinner;outerinnererrinner;";

#[test]
fn private_method_calls_worker() {
    if std::env::var_os("ZIPP_PRIVATE_METHOD_CALLS_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn private_method_calls_match_node() {
    if std::env::var_os("ZIPP_PRIVATE_METHOD_CALLS_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [
        ("default", None),
        ("lanes-off", Some("ZIPP_NO_PRIVATE_CROSS")),
        ("interpreter", Some("ZIPP_NOJIT")),
        ("compile-at-once", Some("ZIPP_JIT_THRESHOLD")),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["private_method_calls_worker", "--exact", "--nocapture"])
            .env("ZIPP_PRIVATE_METHOD_CALLS_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_PRIVATE_CROSS")
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