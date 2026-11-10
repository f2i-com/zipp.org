//! Stamped prototype-chain cache entries (`vm::ic::SiteIc`) against Node: a
//! read, a method call, a `super` member and a constructor's append through a
//! chain answer on one epoch compare, so every change that can move the answer
//! has to move the epoch or fail the receiver-side checks. In-place writes, a
//! nearer link gaining and losing the key, data turned accessor and back, a
//! link's and the receiver's [[Prototype]] replaced, the receiver gaining the
//! key, receivers of one layout with different prototypes, a class instance
//! and a plain object of the same keys at one site, constructor-function
//! methods replaced, shadowed, deleted and re-linked, absent keys on the
//! default and a null prototype, `super` after the parent's method is replaced
//! and the chain re-linked, a class evaluated more than once, prototypes that
//! die and whose slots are reused, a six-link chain changed at each depth, a
//! chain changed while the reading loop runs, and appends after the chain
//! gains a setter or a read-only property of the name. In the interpreter, by
//! default and with every function compiled at once.

const SOURCE: &str = r##""use strict";
const out = [];
function hot(f, n) { let s = 0; for (let i = 0; i < (n || 400); i++) s = (s + f(i)) | 0; return s; }

// 1. a read through a prototype chain, then every change that can move its answer
{
  const root = { deep: 7, label: 3 };
  const mid = Object.create(root); mid.m = 1;
  const leaf = Object.create(mid); leaf.own = 5;
  const rd = (o) => o.deep;
  const r = [];
  r.push(hot(() => rd(leaf)));
  root.deep = 9; r.push(hot(() => rd(leaf)));                       // in-place write on the holder
  mid.deep = 100; r.push(hot(() => rd(leaf)));                      // a nearer link gains the key
  delete mid.deep; r.push(hot(() => rd(leaf)));                     // and loses it
  Object.defineProperty(root, "deep", { get() { return 11; }, configurable: true });
  r.push(hot(() => rd(leaf)));                                      // data becomes an accessor
  Object.defineProperty(root, "deep", { value: 13, writable: true, configurable: true });
  r.push(hot(() => rd(leaf)));
  const other = { deep: 21 };
  Object.setPrototypeOf(mid, other); r.push(hot(() => rd(leaf)));   // a link's prototype replaced
  Object.setPrototypeOf(mid, root); r.push(hot(() => rd(leaf)));
  Object.setPrototypeOf(leaf, other); r.push(hot(() => rd(leaf)));  // the receiver's own link replaced
  Object.setPrototypeOf(leaf, mid); r.push(hot(() => rd(leaf)));
  leaf.deep = 1; r.push(hot(() => rd(leaf)));                       // the receiver gains the key
  delete root.deep; delete leaf.deep; r.push(String(rd(leaf)));     // nowhere on the chain
  root.deep = 2; r.push(hot(() => rd(leaf)));
  out.push("A " + r.join(" "));
}

// 2. one site, receivers of one layout with different prototypes
{
  const p1 = { v: 1 }, p2 = { v: 2 }, p3 = Object.create(p1);
  const mk = (p) => { const o = Object.create(p); o.a = 0; return o; };
  const os = [mk(p1), mk(p2), mk(p3), mk(p1)];
  const rd = (o) => o.v;
  let s = 0;
  for (let i = 0; i < 2000; i++) s += rd(os[i & 3]);
  p3.v = 50;
  for (let i = 0; i < 2000; i++) s += rd(os[i & 3]);
  out.push("B " + s);
}

// 3. a class instance and a plain object of the same keys at one site
{
  class C { constructor() { this.a = 1; } get v() { return 10; } m() { return 100; } }
  const proto = { v: 20, m() { return 200; } };
  const plain = Object.create(proto); plain.a = 1;
  const inst = new C();
  const rd = (o) => o.v + o.m();
  let s = 0;
  for (let i = 0; i < 3000; i++) s += rd(i & 1 ? plain : inst);
  proto.m = function () { return 300; };
  for (let i = 0; i < 3000; i++) s += rd(i & 1 ? plain : inst);
  out.push("C " + s);
}

// 4. inherited methods of constructor functions, replaced, shadowed, deleted and re-linked
{
  function Base() {}
  Base.prototype.area = function () { return 1; };
  function Shape(v) { this.v = v; }
  Shape.prototype = Object.create(Base.prototype);
  Shape.prototype.area = function () { return this.v + 1; };
  const objs = [new Shape(1), new Shape(2), new Shape(3), new Shape(4)];
  const call = (o) => o.area();
  const r = [];
  const run = () => { let s = 0; for (let i = 0; i < 1200; i++) s += call(objs[i & 3]); return s; };
  r.push(run());
  Shape.prototype.area = function () { return this.v * 2; }; r.push(run());      // in place
  objs[1].area = function () { return -5; }; r.push(run());                      // an own method on one
  delete Shape.prototype.area; r.push(run());                                    // falls to Base's
  Base.prototype.area = function () { return 7; }; r.push(run());
  Object.setPrototypeOf(Shape.prototype, { area() { return 9; } }); r.push(run());
  Shape.prototype.area = function () { return 11; }; r.push(run());
  out.push("D " + r.join(" "));
}

// 5. absent keys, then present, on the default and on a null prototype
{
  const o = { a: 1 }, n = Object.create(null); n.a = 1;
  const rd = (x) => x.zzz === undefined ? 0 : x.zzz;
  const r = [];
  r.push(hot(() => rd(o)), hot(() => rd(n)));
  Object.prototype.zzz = 4;
  r.push(hot(() => rd(o)), hot(() => rd(n)));
  delete Object.prototype.zzz;
  r.push(hot(() => rd(o)), hot(() => rd(n)));
  const mid = Object.create(Object.prototype); const leaf = Object.create(mid); leaf.a = 1;
  r.push(hot(() => rd(leaf)));
  mid.zzz = 6; r.push(hot(() => rd(leaf)));
  Object.setPrototypeOf(n, { zzz: 8 }); r.push(hot(() => rd(n)));
  out.push("E " + r.join(" "));
}

// 6. super calls and super reads: the parent's method replaced, the chain re-linked
{
  class Shape { constructor(v) { this._v = v; } area() { return this._v + 1; } get v() { return this._v; } }
  class Circle extends Shape { area() { return super.area() * 3; } get v() { return super.v * 2; } }
  class Hex extends Circle { area() { return super.area() + 1; } }
  const objs = [new Circle(1), new Hex(2), new Circle(3), new Hex(4)];
  const run = () => { let s = 0; for (let i = 0; i < 1600; i++) { const o = objs[i & 3]; s += o.area() + o.v; } return s; };
  const r = [run()];
  Shape.prototype.area = function () { return 50; }; r.push(run());
  Shape.prototype.extra = 1; r.push(run());
  class Other { area() { return 1000; } get v() { return 2000; } }
  Object.setPrototypeOf(Circle.prototype, Other.prototype); r.push(run());
  Object.setPrototypeOf(Circle.prototype, Shape.prototype); r.push(run());
  Circle.prototype.area = function () { return 5; }; r.push(run());
  out.push("F " + r.join(" "));
}

// 7. a class evaluated more than once: each evaluation's super is its own
{
  const make = (k) => {
    class P { m() { return k; } }
    class Q extends P { m() { return super.m() + 1; } }
    return new Q();
  };
  const a = make(10), b = make(20), c = make(30);
  let s = 0;
  for (let i = 0; i < 3000; i++) s += a.m() + b.m() + c.m();
  const d = make(40);
  for (let i = 0; i < 3000; i++) s += a.m() + d.m();
  out.push("G " + s);
}

// 8. prototypes that die and whose slots come back as other objects
{
  let s = 0;
  const rd = (o) => o.v;
  for (let round = 0; round < 40; round++) {
    const p = { v: round }; const o = Object.create(p); o.a = 1;
    for (let i = 0; i < 60; i++) s += rd(o);
    const junk = []; for (let i = 0; i < 400; i++) junk.push({ x: i, y: [i] });
    s += junk.length;
  }
  out.push("H " + s);
}

// 9. six links, and a change at each depth
{
  let o = { deep: 1 }; const chain = [o];
  for (let i = 0; i < 6; i++) { o = Object.create(o); o["k" + i] = i; chain.push(o); }
  const leaf = chain[6];
  const rd = (x) => x.deep;
  const r = [hot(() => rd(leaf))];
  for (let d = 0; d < 6; d++) { chain[d].deep = 10 + d; r.push(hot(() => rd(leaf))); }
  for (let d = 5; d >= 1; d--) { delete chain[d].deep; r.push(hot(() => rd(leaf))); }
  out.push("I " + r.join(" "));
}

// 10. the chain changes while the reading loop runs
{
  const p = { v: 1 }; const q = Object.create(p); const o = Object.create(q); o.a = 0;
  let s = 0;
  for (let i = 0; i < 3000; i++) {
    s += o.v;
    if (i === 1000) q.v = 5;
    if (i === 2000) delete q.v;
    if (i === 2500) p.w = 1;
  }
  const f = { m() { return 1; } }; const g = Object.create(f); g.a = 0;
  for (let i = 0; i < 3000; i++) {
    s += g.m();
    if (i === 1000) f.m = function () { return 2; };
    if (i === 2000) Object.setPrototypeOf(g, { m() { return 3; } });
  }
  out.push("J " + s);
}
// 11. a constructor's appends, then the chain gains a setter and a read-only property of the name
{
  function Pt(x, y) { this.x = x; this.y = y; }
  const mk = () => { let s = 0; for (let i = 0; i < 300; i++) { const p = new Pt(i, 2); s += p.x + p.y + Object.keys(p).length; } return s; };
  const r = [mk()];
  let hits = 0;
  Object.defineProperty(Pt.prototype, "y", { set(v) { hits++; }, get() { return 100; }, configurable: true });
  r.push(mk(), hits);
  delete Pt.prototype.y;
  r.push(mk());
  Object.defineProperty(Object.prototype, "x", { value: 7, writable: false, configurable: true });
  let threw = 0, s = 0;
  for (let i = 0; i < 300; i++) { try { const p = new Pt(i, 2); s += p.x; } catch (e) { threw++; } }
  r.push(s, threw);
  delete Object.prototype.x;
  r.push(mk());
  const base = { z: 1 };
  Object.setPrototypeOf(Pt.prototype, base);
  r.push(mk());
  Object.defineProperty(base, "x", { set(v) { hits += 1000; }, configurable: true });
  r.push(mk(), hits);
  out.push("K " + r.join(" "));
}

// 12. appends to objects with no prototype, and to one whose prototype is then replaced
{
  const mkn = (p) => { let s = 0; for (let i = 0; i < 300; i++) { const o = Object.create(p); o.a = i; o.b = 1; s += o.a + o.b; } return s; };
  const r = [mkn(null)];
  const p = { c: 1 }; r.push(mkn(p));
  let seen = 0;
  Object.defineProperty(p, "b", { set(v) { seen += v; }, configurable: true });
  r.push(String(mkn(p)), seen, mkn(null));
  out.push("N " + r.join(" "));
}
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "A 2800 3600 40000 3600 4400 5200 8400 5200 8400 5200 400 undefined 800\nB 29500\nC 1140000\nD 4200 6000 3300 -600 4800 6600 8400\nE 0 0 1600 0 0 0 0 2400 3200\nF 25600 248800 248800 11200800 248800 16800\nG 345000\nH 62800\nI 400 4000 4400 4800 5200 5600 6000 5600 5200 4800 4400 4000\nJ 12998\nK 46050 75150 300 46050 0 300 46050 46050 NaN 300300\nN 45150 45150 NaN 300 45150";

#[test]
fn chain_stamps_worker() {
    if std::env::var_os("ZIPP_CHAIN_STAMPS_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn chain_stamps_match_node() {
    if std::env::var_os("ZIPP_CHAIN_STAMPS_WORKER").is_some() {
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
            .args(["chain_stamps_worker", "--exact", "--nocapture"])
            .env("ZIPP_CHAIN_STAMPS_WORKER", "1")
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
