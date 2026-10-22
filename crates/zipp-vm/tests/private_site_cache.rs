//! A private-name site remembers, in its cache's first way, the field its
//! code resolved `#x` to while running as one callee (`IcEntry::Private`),
//! and the own hit answers from it. Every way that answer could leak: the
//! same method code under two evaluations of one class expression (each its
//! own brand), the field read and written through another class's object,
//! a hierarchy whose levels each read their own fields, fields stamped on
//! arbitrary objects through a constructor's return override, statics,
//! a shadowing inner class, arrows inside methods, destructuring and
//! logical-assignment targets, a Proxy and a frozen object, `#x in`, and
//! hundreds of evaluations collected in between. The expected lines are
//! Node's (v24), in the JIT, the interpreter and the interpreter under GC
//! stress.

const SOURCE: &str = r###"
// Private-name site cache probe: every way a cached (callee, brand, field)
// answer could leak across class evaluations, receivers or callees.
const out = [];
function t(name, f) {
  try { out.push(name + ": " + f()); } catch (e) { out.push(name + ": " + e.constructor.name); }
}

// 1. One class expression evaluated twice: same code, two brands.
function make(v) {
  return class {
    #x = v;
    get() { return this.#x; }
    set(n) { this.#x = n; return this.#x; }
    bump() { this.#x += 1; this.#x++; return this.#x; }
    static read(o) { return o.#x; }
  };
}
const A = make(1), B = make(2);
const a = new A(), b = new B();
for (let i = 0; i < 50; i++) { a.get(); b.get(); a.set(i); b.bump(); }
t("own a", () => a.get());
t("own b", () => b.get());
t("A.read(a)", () => A.read(a));
t("A.read(b) after warm", () => { for (let i = 0; i < 20; i++) A.read(a); return A.read(b); });
t("B.read(a)", () => B.read(a));
t("A get on b", () => A.prototype.get.call(b));
t("A set on b", () => A.prototype.set.call(b, 9));
t("A bump on b", () => A.prototype.bump.call(b));
t("b after", () => b.get());

// 2. A hierarchy: base fields read through subclass instances, and a
// subclass method reading its own and (via super) the base's.
class Base { #p = 10; p() { return this.#p; } setp(v) { this.#p = v; } }
class Mid extends Base { #q = 20; q() { return this.#q + this.p(); } }
class Leaf extends Mid { #r = 30; r() { return this.#r + this.q(); } }
const objs = [new Base(), new Mid(), new Leaf(), new Leaf(), new Mid()];
let s = 0;
for (let i = 0; i < 200; i++) { const o = objs[i % 5]; s += o.p(); if (o instanceof Mid) s += o.q(); if (o instanceof Leaf) s += o.r(); o.setp(i & 7); }
t("hierarchy sum", () => s);
t("Mid.q on Base", () => Mid.prototype.q.call(new Base()));
t("Leaf.r on Mid", () => Leaf.prototype.r.call(new Mid()));

// 3. Return-override stamping: private fields on arbitrary objects of many shapes.
class Ret { constructor(o) { return o; } }
class Stamp extends Ret {
  #tag = "s";
  constructor(o) { super(o); }
  static tag(o) { return o.#tag; }
  static retag(o, v) { o.#tag = v; return o.#tag; }
}
const plain = [{}, { a: 1 }, { a: 1, b: 2 }, [], function () {}, new Map()];
for (const o of plain) new Stamp(o);
const tags = [];
for (let i = 0; i < 60; i++) { const o = plain[i % plain.length]; tags.push(Stamp.tag(o)); if (i % 7 === 0) Stamp.retag(o, "t" + i); }
t("stamped tags", () => tags.slice(-6).join(","));
t("unstamped", () => Stamp.tag({ a: 1 }));
t("stamp twice", () => { new Stamp(plain[1]); return "no"; });

// 4. Static private fields and methods.
class St {
  static #n = 0;
  static #m() { return St.#n * 2; }
  static inc() { St.#n++; return St.#m(); }
  static peek(o) { return o.#n; }
}
for (let i = 0; i < 30; i++) St.inc();
t("static", () => St.inc());
t("static on subclass", () => { class Sub extends St {} return St.peek(Sub); });

// 5. Shadowing: an inner class's #x is not the outer's.
class Outer {
  #x = "outer";
  make() {
    const outerX = () => this.#x;
    return class Inner {
      #x = "inner";
      both(o) { return this.#x + "/" + outerX(); }
      steal(o) { return o.#x; }
    };
  }
  read(o) { return o.#x; }
}
const outer = new Outer();
const Inner = outer.make();
const inner = new Inner();
for (let i = 0; i < 20; i++) { inner.both(); outer.read(outer); }
t("outer reads inner", () => outer.read(inner));
t("shadow both", () => inner.both());
t("inner steal outer", () => inner.steal(outer));
t("inner steal inner", () => inner.steal(inner));

// 6. Arrows and nested functions inside methods share the method's privates.
class Arr {
  #v = 5;
  run() { const f = () => this.#v; const g = (o) => { o.#v += 1; return o.#v; }; let r = 0; for (let i = 0; i < 10; i++) r += f() + g(this); return r; }
}
t("arrows", () => new Arr().run());

// 7. Destructuring targets and update forms.
class D {
  #a = 1; #b = 2;
  swap() { [this.#a, this.#b] = [this.#b, this.#a]; return this.#a * 10 + this.#b; }
  upd() { this.#a ||= 7; this.#b &&= 9; this.#a ??= 0; this.#a **= 2; return this.#a + ":" + this.#b; }
}
const d = new D();
for (let i = 0; i < 9; i++) d.swap();
t("swap", () => d.swap());
t("logical assign", () => d.upd());

// 8. A Proxy and a frozen object as receivers.
class F { #z = 3; z() { return this.#z; } setz(v) { this.#z = v; return this.#z; } }
const f = new F();
for (let i = 0; i < 20; i++) f.z();
t("proxy read", () => F.prototype.z.call(new Proxy(f, {})));
t("frozen write", () => { Object.freeze(f); return f.setz(4); });
t("frozen read", () => f.z());

// 9. `in` brand checks next to cached reads.
class H { #h = 1; static has(o) { return #h in o; } read(o) { return o.#h; } }
const h = new H();
for (let i = 0; i < 20; i++) h.read(h);
t("in", () => [H.has(h), H.has({}), H.has(new Proxy(h, {}))].join(","));

// 10. Many evaluations, collected between: heads must not be reused.
let total = 0;
for (let round = 0; round < 300; round++) {
  const C = make(round);
  const c = new C();
  total += c.get() + c.bump();
  if (round % 50 === 0) { const junk = []; for (let i = 0; i < 20000; i++) junk.push({ i }); total += junk.length & 1; }
}
t("rounds", () => total);
t("old A after rounds", () => a.get() + "," + A.read(a));
t("old A on new", () => A.read(new (make(0))()));

console.log(out.join("\n"));
"###;

const EXPECTED: &str = "own a: 49\nown b: 102\nA.read(a): 49\nA.read(b) after warm: TypeError\nB.read(a): TypeError\nA get on b: TypeError\nA set on b: TypeError\nA bump on b: TypeError\nb after: 102\nhierarchy sum: 8792\nMid.q on Base: TypeError\nLeaf.r on Mid: TypeError\nstamped tags: t42,t49,t14,t21,t28,t35\nunstamped: TypeError\nstamp twice: TypeError\nstatic: 62\nstatic on subclass: TypeError\nouter reads inner: TypeError\nshadow both: inner/outer\ninner steal outer: TypeError\ninner steal inner: inner\narrows: 200\nswap: 12\nlogical assign: 1:9\nproxy read: TypeError\nfrozen write: 4\nfrozen read: 4\nin: true,false,false\nrounds: 90300\nold A after rounds: 49,49\nold A on new: TypeError";

#[test]
fn private_site_cache_worker() {
    if std::env::var_os("ZIPP_PRIVATE_SITE_CACHE_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn private_site_cache_matches_node() {
    if std::env::var_os("ZIPP_PRIVATE_SITE_CACHE_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, nojit, gc) in [("default", false, false), ("interpreter", true, false), ("interpreter-gc-stress", true, true)] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["private_site_cache_worker", "--exact", "--nocapture"])
            .env("ZIPP_PRIVATE_SITE_CACHE_WORKER", "1")
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
