//! `new D(...)` of a derived class and `super(...)`, from hot code.
//!
//! Compiled code now makes both without the general call: a compiled `new`
//! of a derived class with a constructor of its own pushes that
//! constructor's frame from the caller's window (`jit_new_derived_class`),
//! and `super(...)` to a class parent with a constructor of its own enters
//! the parent's body from the calling constructor's window
//! (`super_parent_direct`), frame-free when the parent is a base class whose
//! body cannot see `new.target`. Every other parent takes `run_class_ctor`
//! as before.
//!
//! The program runs each shape of construction three thousand times: a
//! five-level hierarchy; fewer and more arguments than parameters, rest
//! and default parameters and `arguments` in the parent; `new.target`, an
//! arrow that captures `this`, fields and private names at every level;
//! what a constructor may return and what it must have done first; a
//! parent that throws, caught and not; parents that are a built-in, an ES5
//! constructor, a bound function, a Proxy, a class with no constructor of
//! its own, and null; `Reflect.construct` with another `new.target`; a
//! class whose prototype changes while its sites are hot; a class
//! expression evaluated three times; and construction inside construction.
//!
//! The expected lines are Node 24's.

const PROGRAM: &str = r#"
const out = [];
const log = (...a) => out.push(a.join(" "));
const N = 3000;
function hot(fn) { let last; for (let i = 0; i < N; i++) last = fn(i); return last; }
function err(fn) { try { fn(); return "no error"; } catch (e) { return e.constructor.name + (e instanceof ReferenceError || e instanceof TypeError ? "" : ":" + e.message); } }

// A five-level hierarchy with constructors of their own, arguments threaded through.
class L0 { constructor(a, b) { this.a = a; this.b = b; this.order = "0"; } tag() { return "L0"; } }
class L1 extends L0 { constructor(a, b, c) { super(a, b); this.c = c; this.order += "1"; } }
class L2 extends L1 { constructor(a, b, c) { super(a, b + 1, c); this.d = a + b; this.order += "2"; } }
class L3 extends L2 { constructor(a, b) { super(a, b, "c" + a); this.e = [a]; this.order += "3"; } }
class L4 extends L3 { constructor(a) { super(a, a * 2); this.f = { a }; this.order += "4"; } tag() { return "L4:" + super.tag(); } }
log("deep", hot((i) => { const o = new L4(i); return [o.a, o.b, o.c, o.d, o.e[0], o.f.a, o.order, o.tag(), o instanceof L0, Object.keys(o).join("")].join(","); }));

// Fewer and more arguments than parameters, a rest parameter, defaults and `arguments` in the parent.
class R0 { constructor(x, ...rest) { this.x = x; this.n = rest.length; this.sum = rest.reduce((s, v) => s + v, 0); } }
class R1 extends R0 { constructor(...all) { super(...all); this.count = all.length; } }
class R2 extends R0 { constructor(a, b, c, d) { super(a, b, c, d, 5); } }
class D0 { constructor(p = 7, q = p + 1) { this.p = p; this.q = q; this.argc = arguments.length; } }
class D1 extends D0 { constructor(v) { if (v & 1) super(); else super(v, undefined); } }
log("args", hot((i) => { const a = new R1(i, 1, 2, 3), b = new R2(i, 1), c = new D1(i); return [a.x, a.n, a.sum, a.count, b.n, b.sum, c.p, c.q, c.argc].join(","); }));

// new.target, an arrow that captures `this`, fields and private names at every level.
class F0 { base = "f0"; #secret = 40; constructor() { this.nt = new.target.name; this.get = () => this.base + this.#secret; } static check(o) { return #secret in o; } }
class F1 extends F0 { mid = this.base + ">f1"; #more = 2; constructor(k) { super(); this.k = k; this.total = () => this.#more + k; } }
class F2 extends F1 { top = this.mid + ">f2"; constructor(k) { super(k + 1); this.self = new.target === F2; } }
log("fields", hot((i) => { const o = new F2(i); return [o.nt, o.get(), o.mid, o.top, o.k, o.total(), o.self, F0.check(o)].join(","); }));

// What a constructor returns, and what it must have done.
class Base { constructor(mode) { this.kind = "base"; if (mode === 1) return { kind: "override" }; if (mode === 2) return 5; } }
class Keep extends Base { constructor(mode) { super(mode); this.kept = true; } }
class RetObj extends Base { constructor() { super(0); return { kind: "derived-override" }; } }
class RetPrim extends Base { constructor() { super(0); return 1; } }
class NoSuper extends Base { constructor(skip) { if (!skip) super(0); } }
class Twice extends Base { constructor(twice) { super(0); if (twice) super(0); } }
class Early extends Base { constructor(early) { if (early) this.x = 1; super(0); } }
class Retry extends Base { constructor(fail) { try { if (fail) throw new Error("first"); super(0); } catch (e) { super(0); this.retried = e.message; } } }
log("returns", hot((i) => [new Keep(0).kind, new Keep(0).kept, new Keep(1).kind, new Keep(1).kept, new Keep(2).kind, new RetObj().kind, new NoSuper(false).kind, new Twice(false).kind, new Early(false).kind, new Retry(i & 1).retried].join(",")));
log("errors", err(() => hot(() => new RetPrim())), err(() => hot((i) => new NoSuper(i === N - 1))), err(() => hot((i) => new Twice(i === N - 1))), err(() => hot((i) => new Early(i === N - 1))));

// A parent that throws, with and without a handler in the child.
class Thrower { constructor(i) { if (i % 1000 === 999) throw new RangeError("boom" + i); this.i = i; } }
class Catches extends Thrower { constructor(i) { try { super(i); } catch (e) { super(0); this.caught = e.message; } } }
class Passes extends Thrower { constructor(i) { super(i); this.after = true; } }
let caught = 0; hot((i) => { const o = new Catches(i); if (o.caught) caught++; return o; });
log("throws", caught, err(() => hot((i) => new Passes(i))));

// Parents of other kinds: a built-in, an ES5 constructor, a bound function, a Proxy, a class with no constructor, null.
function Old(a) { this.old = a; this.nt = new.target === Old ? "Old" : new.target && new.target.name; }
Old.prototype.who = function () { return "old" + this.old; };
class FromOld extends Old { constructor(a) { super(a + 1); this.young = true; } }
class FromErr extends Error { constructor(m) { super("wrapped:" + m); this.code = 7; } }
class FromArr extends Array { constructor(n) { super(n); this.extra = "x"; } }
class FromMap extends Map { constructor(k) { super([[k, 1]]); this.label = "m"; } }
const Bound = Base.bind(null, 0); Bound.prototype = Base.prototype;
class FromBound extends Bound { constructor() { super(); this.b = 1; } }
function OldF(a, b) { this.a = a; this.b = b; }
const BoundOld = OldF.bind({ ignored: true }, 1); BoundOld.prototype = OldF.prototype;
class FromBoundOld extends BoundOld { constructor() { super(2); this.c = 3; } }
const Proxied = new Proxy(Base, { construct(t, args, nt) { const o = Reflect.construct(t, args, nt); o.viaProxy = true; return o; } });
class FromProxy extends Proxied { constructor() { super(0); this.p = 1; } }
class Implicit extends Base {}
class OverImplicit extends Implicit { constructor(m) { super(m); this.o = 1; } }
class FromNull extends null { constructor() { super(); } }
log("kinds", hot((i) => { const a = new FromOld(i), e = new FromErr("m" + i), r = new FromArr(3), m = new FromMap(i), b = new FromBound(), p = new FromProxy(), o = new OverImplicit(0), bo = new FromBoundOld(); return [bo.a, bo.b, bo.c, bo.ignored, bo instanceof FromBoundOld, b instanceof FromBound, a.old, a.nt, a.who(), a.young, e.message, e.code, e instanceof Error, r.length, r.extra, Array.isArray(r), m.get(i), m.label, b.kind, b.b, p.kind, p.viaProxy, p.p, p instanceof FromProxy, o.kind, o.o].join(","); }));
log("null", err(() => new FromNull()));

// new.target through super(...) with Reflect.construct, and a prototype changed after the sites are hot.
class Seen { constructor() { this.seen = new.target.name; } }
class SeenChild extends Seen { constructor() { super(); this.child = new.target.name; } }
class Other { constructor() { this.other = true; } }
function Target() {} Target.prototype = { marker: "target-proto" };
log("reflect", hot(() => { const o = Reflect.construct(SeenChild, [], Target); return [o.seen, o.child, o.marker, o instanceof SeenChild].join(","); }));
log("before", hot(() => { const o = new SeenChild(); return [o.seen, o.child, o.other].join(","); }));
Object.setPrototypeOf(SeenChild, Other);
log("after", hot(() => { const o = new SeenChild(); return [o.seen, o.child, o.other, o instanceof Seen].join(","); }));

// A class expression evaluated more than once: each evaluation's super(...) reaches its own parent.
function make(tag, Parent) { return class extends Parent { constructor(v) { super(v); this.tags = (this.tags || "") + tag; } }; }
class Root { constructor(v) { this.v = v; } }
const A = make("a", Root), B = make("b", A), C = make("c", B);
log("mixins", hot((i) => { const o = new C(i); return [o.v, o.tags, o instanceof A, o instanceof B].join(","); }));

// Construction inside construction, and instances kept across collections.
class Node0 { constructor(depth) { this.depth = depth; this.kids = []; } }
class Tree extends Node0 { constructor(depth) { super(depth); if (depth < 3) { this.kids.push(new Tree(depth + 1)); this.kids.push(new Tree(depth + 1)); } this.size = 1 + this.kids.reduce((s, k) => s + k.size, 0); } }
const keep = []; hot((i) => { const t = new Tree(0); if (i % 500 === 0) keep.push(t); return t.size; });
log("nested", keep.length, keep.map((t) => t.size + ":" + t.kids[1].kids[0].depth).join(","));
console.log(out.join("\n"));
"#;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output.iter().flat_map(|l| l.lines().map(str::to_string)).collect()
}

#[test]
fn derived_construction_answers_as_node() {
    assert_eq!(
        run_ok(PROGRAM),
        [
            "deep 2999,5999,c2999,8997,2999,2999,01234,L4:L0,true,abordercdef",
            "args 2999,3,6,4,4,NaN,7,8,0",
            "fields F2,f040,f0>f1,f0>f1>f2,3000,3002,true,true",
            "returns base,true,override,true,base,derived-override,base,base,base,first",
            "errors TypeError ReferenceError ReferenceError ReferenceError",
            "throws 3 RangeError:boom999",
            "kinds 1,2,3,,true,true,3000,FromOld,old3000,true,wrapped:m2999,7,true,3,x,true,1,m,base,1,base,true,1,true,base,1",
            "null TypeError",
            "reflect Target,Target,target-proto,false",
            "before SeenChild,SeenChild,",
            "after ,SeenChild,true,true",
            "mixins 2999,abc,true,true",
            "nested 6 15:2,15:2,15:2,15:2,15:2,15:2",
        ]
    );
}

/// The same answers with the JIT compiling at once, with a collection at
/// every safe point, and with no native code.
#[test]
fn derived_construction_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("stress", "ZIPP_GC_STRESS", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("derived_construction_answers")
            .arg("--nocapture")
            .env(key, value)
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the filter matched nothing in {name} mode"
        );
    }
}
