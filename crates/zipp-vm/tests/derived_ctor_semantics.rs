//! Derived-class constructors run hot, against Node: `super(...)` and
//! `super(...spread)` through three levels with fields and `new.target`, a
//! parent returning an object (return override), `this` read before
//! `super()`, `super()` twice, a primitive or missing return, `super()` from
//! an arrow (whose `new.target` is the construction's: the parent used to see
//! `undefined`, as the arrow's own frame has none) and that arrow called
//! twice, `Reflect.construct` with a foreign `newTarget`, `extends null`, a
//! plain-function parent, throws from a parent constructor and a field
//! initializer, and a recursive constructor 60 deep. In the
//! interpreter, by default and with every function compiled at once.

const SOURCE: &str = r##""use strict";
// Derived-class constructors (super(), this-TDZ, new.target) run hot enough to compile.
const out = [];
function hot(n, f) { let s = 0; for (let i = 0; i < n; i++) s = (s + f(i)) | 0; return s; }
const name = (e) => e.constructor.name;
// 1. a plain hierarchy with fields, new.target and spread super
class A { #a = 1; x = 2; constructor(v) { this.v = v; this.nt = new.target.name; } get a() { return this.#a; } }
class B extends A { #b = 3; y = 4; constructor(v, w) { super(v); this.w = w + this.#b; } }
class C extends B { constructor(...xs) { super(...xs); this.c = new.target === C; } }
class D extends A {}
out.push("H " + hot(4000, (i) => { const c = new C(i, 1), d = new D(i); return c.v + c.w + c.y + c.x + c.a + d.v + (c.c ? 1 : 0) + c.nt.length + d.nt.length; }));
// 2. a parent returning an object (return override): `this` is that object, fields land on it
class R { constructor(o) { return o; } }
class S extends R { #s = 5; z = 6; constructor(o) { super(o); this.after = true; } static s(o) { return o.#s; } }
out.push("R " + hot(3000, (i) => { const o = { i }; const r = new S(o); return (r === o ? 1 : 0) + r.z + S.s(o) + (o.after ? 1 : 0) + r.i; }));
// 3. this before super(), super() twice, returning a primitive / nothing without super()
class T1 extends A { constructor() { this.x = 1; super(0); } }
class T2 extends A { constructor() { super(0); super(1); } }
class T3 extends A { constructor() { return 5; } }
class T4 extends A { constructor() { } }
class T5 extends A { constructor() { super(1); return { other: 1 }; } }
const e3 = [];
for (let i = 0; i < 3000; i++) {
  for (const K of [T1, T2, T3, T4, T5]) {
    try { const o = new K(); if (i === 0) e3.push(K.name + ":" + (o.other ? "obj" : o.v)); } catch (e) { if (i === 0) e3.push(K.name + ":" + name(e)); }
  }
}
out.push("T " + e3.join(" "));
// 4. an arrow that calls super(), and one that escapes before it
class U extends A { constructor() { const f = () => super(7); f(); try { f(); } catch (e) { this.twice = name(e); } } }
out.push("U " + hot(3000, (i) => new U().v) + " " + new U().twice);
// 5. new.target through Reflect.construct, and in the base constructor
class NT extends A { constructor() { super(0); this.t = new.target; } }
function Fake() {}
out.push("N " + hot(3000, (i) => { const o = Reflect.construct(NT, [], i & 1 ? Fake : NT); return (o.t === Fake ? 1 : 0) + (Object.getPrototypeOf(o) === (i & 1 ? Fake.prototype : NT.prototype) ? 10 : 0) + o.nt.length; }));
// 7. extends null, a plain-function parent, a parent that throws, a throwing field initializer
class Nul extends null { constructor() { return Object.create(Nul.prototype); } }
function FP(x) { this.fp = x; }
class W extends FP { constructor(x) { super(x * 2); } }
class Thr { constructor(x) { if (x === 13) throw new RangeError("13"); } }
class X extends Thr { q = (() => { if (this.k === 7) throw new TypeError("k"); return 1; })(); constructor(x) { super(x); } }
let e7 = [0, 0];
const w7 = hot(3000, (i) => {
  const n = new Nul(); const w = new W(i);
  try { new X(i % 20); } catch (e) { e7[e instanceof RangeError ? 0 : 1]++; }
  return (n instanceof Nul ? 1 : 0) + w.fp;
});
out.push("W " + w7 + " " + e7.join(","));
// 8. a recursive derived constructor, 60 deep
class Deep extends A { constructor(n) { super(n); this.next = n > 0 ? new Deep(n - 1) : null; } }
let depth = 0;
for (let i = 0; i < 50; i++) { let d = new Deep(60); depth = 0; while (d) { depth++; d = d.next; } }
out.push("D " + depth);
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "H 16052000\nR 4537500\nT T1:ReferenceError T2:ReferenceError T3:TypeError T4:ReferenceError T5:obj\nU 21000 ReferenceError\nN 40500\nW 9000000 150,0\nD 61";

#[test]
fn derived_ctor_semantics_worker() {
    if std::env::var_os("ZIPP_DERIVED_CTOR_SEMANTICS_WORKER").is_none() {
        return;
    }
    // Nested constructions nest run loops, whose frames a debug build makes
    // large: the default 2 MiB test thread is too small for a few dozen.
    let out = std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(|| zipp_vm::run(SOURCE).expect("source compiles"))
        .expect("spawn the VM thread")
        .join()
        .expect("the VM thread");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn derived_ctor_semantics_match_node() {
    if std::env::var_os("ZIPP_DERIVED_CTOR_SEMANTICS_WORKER").is_some() {
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
            .args(["derived_ctor_semantics_worker", "--exact", "--nocapture"])
            .env("ZIPP_DERIVED_CTOR_SEMANTICS_WORKER", "1")
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