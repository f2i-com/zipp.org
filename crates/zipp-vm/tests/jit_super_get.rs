//! `super.name` read from methods compiled whole (Tier C), against Node.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"m 6004
data 8
getter 30
missing undefined
static 5999
static getter static-getter
data after write 100
data became getter 1004
method replaced -10
before D>C
retargeted D>X
caught caught boom f
escaped boom f
repeated P0!,P1!,P2!,P3!,P4! P39!
arrow object/number
object proto function
null base TypeError
chain 3"####;

const PROGRAM: &str = r####"// `super.name` in methods hot enough to compile, every kind of answer it can give.
function hot(f, n = 3000) { let r; for (let i = 0; i < n; i++) r = f(i); return r; }
const out = [];
const log = (...a) => out.push(a.join(" "));

class A {
  constructor() { this.v = 1; }
  m(x) { return x + this.v; }
  get g() { return this.v * 10; }
  static s(x) { return x * 2; }
  static get sg() { return "static-getter"; }
}
A.prototype.data = 7;
class B extends A {
  constructor() { super(); this.v = 3; }
  m(x) { return super.m(x) * 2; }
  rd(i) { return super.data + (i & 1); }
  gg() { return super.g; }
  miss() { return super.nope; }
  static s(x) { return super.s(x) + 1; }
  static sg2() { return super.sg; }
}
const b = new B();
log("m", hot(i => b.m(i)));
log("data", hot(i => b.rd(i)));
log("getter", hot(() => b.gg()));
log("missing", String(hot(() => b.miss())));
log("static", hot(i => B.s(i)));
log("static getter", hot(() => B.sg2()));

// The parent changes after the methods compiled: data, then a getter, then a method.
A.prototype.data = 100;
log("data after write", b.rd(0));
Object.defineProperty(A.prototype, "data", { get() { return this.v + 1000; }, configurable: true });
log("data became getter", b.rd(1));
A.prototype.m = function (x) { return -x; };
log("method replaced", b.m(5));

// The super base changes under a compiled method.
class C { m() { return "C"; } }
class D extends C { m() { return "D>" + super.m(); } }
const d = new D();
log("before", hot(() => d.m()));
Object.setPrototypeOf(D.prototype, { m() { return "X"; } });
log("retargeted", d.m());

// A getter that throws, caught inside the compiled method and outside it.
class E { get boom() { throw new Error("boom " + this.tag); } }
class F extends E {
  constructor(t) { super(); this.tag = t; }
  safe() { try { return super.boom; } catch (e) { return "caught " + e.message; } }
  unsafe() { return super.boom; }
}
const f = new F("f");
log("caught", hot(() => f.safe()));
try { hot(() => f.unsafe(), 10); } catch (e) { log("escaped", e.message); }

// A class expression evaluated many times: each evaluation's own parent.
function make(k) {
  const P = class { who() { return "P" + k; } };
  return class extends P { who() { return super.who() + "!"; } };
}
const made = [];
for (let k = 0; k < 40; k++) made.push(new (make(k))());
let s = "";
for (let r = 0; r < 200; r++) for (const o of made) s = o.who();
log("repeated", made.map(o => o.who()).slice(0, 5).join(","), s);

// `super.x` with the receiver a primitive-backed method call, and in an arrow.
class G { k() { return typeof this; } }
class H extends G { k() { const f = () => super.k(); return f() + "/" + super.k.call(5); } }
log("arrow", hot(() => new H().k()));

// A base class whose prototype's parent is null: RequireObjectCoercible.
class N { m() { return super.toString; } }
log("object proto", typeof hot(() => new N().m()));
Object.setPrototypeOf(N.prototype, null);
try { new N().m(); log("null base read", "no throw"); } catch (e) { log("null base", e.constructor.name); }

// Deep chains and symbols.
const sym = Symbol("s");
class L0 { [sym]() { return 0; } n() { return 0; } }
class L1 extends L0 { n() { return super.n() + 1; } }
class L2 extends L1 { n() { return super.n() + 1; } }
class L3 extends L2 { n() { return super.n() + 1; } }
log("chain", hot(() => new L3().n()));
console.log(out.join("\n"));
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
