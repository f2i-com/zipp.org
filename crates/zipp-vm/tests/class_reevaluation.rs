//! A class definition evaluated more than once (a class expression in a
//! function called twice, a mixin applied to two bases) makes a class per
//! evaluation, and each one's code must see its own: `super.m()`,
//! `super.x` reads and writes, `super` in arrows, field initializers and
//! static methods, the class's own name inside its methods, and a computed
//! field key. `class_values` remembers only the latest evaluation, and every
//! one of these resolved against it, so an earlier class's methods ran the
//! newest parent's (a private field there threw a brand `TypeError`, a
//! public one returned the wrong value). Classes made per round past the
//! inline caches' miss limit (`IC_PROBATION`) must keep their answers too.
//! The expected lines are Node's (v24), in the JIT, the interpreter and the
//! interpreter under GC stress.

const SOURCE: &str = r##"
"use strict";
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function catchName(f) { try { return f(); } catch (e) { return e.constructor.name + ": " + e.message; } }

// super property references in a class whose parent is made per call.
function pair(i) {
  class Base {
    #b = i;
    get g() { return i; }
    set s(v) { this.got = v + i; }
    m() { return this.#b; }
    static sm() { return i; }
  }
  return class extends Base {
    m() { return super.m(); }
    get g() { return super.g; }
    put(v) { super.s = v; return this.got; }
    arrow() { return (() => super.m())(); }
    f = super.g;
    static sm() { return super.sm(); }
  };
}
var A = pair(1), B = pair(2);
var a = new A(), b = new B();
log("super", a.m(), b.m(), a.g, b.g, a.put(10), b.put(10), a.arrow(), b.arrow(), a.f, b.f, A.sm(), B.sm());

// A mixin applied to two bases.
const Mix = Sup => class extends Sup { who() { return "mix+" + super.who(); } };
class P { who() { return "p"; } }
class Q { who() { return "q"; } }
const MP = Mix(P), MQ = Mix(Q);
log("mixin", new MP().who(), new MQ().who(), new MP().who());

// The class's own name inside its methods, and a computed field key.
function named(i, k) {
  return class C { static id = i; [k] = i; who() { return C.id; } static whoS() { return C.id; } };
}
var C1 = named(1, "x"), C2 = named(2, "y");
log("name", new C1().who(), new C2().who(), C1.whoS(), C2.whoS(), Object.keys(new C1()).join(), Object.keys(new C2()).join());

// Many evaluations, older ones used after newer ones exist, in hot loops.
var ks = [];
for (var i = 0; i < 12; i++) ks.push(pair(i));
var objs = ks.map(function (K) { return new K(); });
var h = 0;
for (var r = 0; r < 20000; r++) {
  var o = objs[(r * 5) % 12];
  h = (h + o.m() + o.g + o.arrow()) | 0;
}
log("hot", h);

// Classes made per round, past the inline caches' miss limit: getters,
// setters, methods and statics keep their answers.
function cell(i) {
  return class { static made = 0; #v = i; get v() { return this.#v; } set v(x) { this.#v = x; } swap(o) { var t = this.#v; this.#v = o.#v; o.#v = t; } static has(o) { return #v in o; } };
}
var s = 0;
for (var round = 0; round < 300; round++) {
  var K = cell(round), xs = [new K(), new K(), new K()];
  for (var j = 0; j < 40; j++) { var p = xs[j % 3]; p.swap(xs[(j + 1) % 3]); p.v = p.v + 1; s = (s + p.v + K.made++) | 0; }
  if (!K.has(xs[0])) s ^= 1;
}
log("churn", s);
log("brand", catchName(function () { return A.prototype.m.call(b); }));
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "super 1 2 1 2 11 12 1 2 1 2 1 2\nmixin mix+p mix+q mix+p\nname 1 2 1 2 x y\nhot 330000\nchurn 2154000\nbrand TypeError: Cannot read private member #b from an object whose class did not declare it";

#[test]
fn class_reevaluation_worker() {
    if std::env::var_os("ZIPP_CLASS_REEVAL_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn class_reevaluation_matches_node() {
    if std::env::var_os("ZIPP_CLASS_REEVAL_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, nojit, gc) in [("default", false, false), ("interpreter", true, false), ("interpreter-gc-stress", true, true)] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["class_reevaluation_worker", "--exact", "--nocapture"])
            .env("ZIPP_CLASS_REEVAL_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS");
        if nojit {
            command.env("ZIPP_NOJIT", "1");
        }
        if gc {
            command.env("ZIPP_GC_STRESS", "1");
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