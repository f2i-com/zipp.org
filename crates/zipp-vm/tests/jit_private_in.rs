//! `#x in obj` in functions compiled whole (Tier C), against Node.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"field true,true,false,false,false,false,false
method true,true,false,false,false,false,false
getter true,true,false,false,false,false,false
static true false false
subclass true false
both true
counted 5715
prim number TypeError
prim string TypeError
prim object TypeError
prim undefined TypeError
prim symbol TypeError
prim bigint TypeError
repeated true false true false
before stamp false
after stamp true true"####;

const PROGRAM: &str = r####"// `#x in obj` in functions hot enough to compile whole: fields, methods, accessors,
// static members, foreign objects, primitives, proxies, repeated class expressions.
function hot(f, n = 3000) { let r; for (let i = 0; i < n; i++) r = f(i); return r; }
const out = [];
const log = (...a) => out.push(a.join(" "));

class A {
  #f = 1;
  #m() { return 1; }
  get #g() { return 2; }
  static #s = 3;
  static hasF(o) { return #f in o; }
  static hasM(o) { return #m in o; }
  static hasG(o) { return #g in o; }
  static hasS(o) { return #s in o; }
  static both(a, b) { return #f in a && #m in b; }
}
class B extends A { #own = 0; static hasOwn(o) { return #own in o; } }
const a = new A(), b = new B(), plain = { f: 1 };
const objs = [a, b, plain, [], function () {}, new Proxy(a, {}), Object.create(a)];
log("field", objs.map(o => hot(() => A.hasF(o), 500)).join());
log("method", objs.map(o => hot(() => A.hasM(o), 500)).join());
log("getter", objs.map(o => hot(() => A.hasG(o), 500)).join());
log("static", hot(() => A.hasS(A)), hot(() => A.hasS(B)), hot(() => A.hasS(a)));
log("subclass", hot(() => B.hasOwn(b)), hot(() => B.hasOwn(a)));
log("both", hot(i => A.both(i & 1 ? a : b, i & 2 ? b : plain)));
let counted = 0;
for (let i = 0; i < 20000; i++) if (A.hasF(objs[i % objs.length])) counted++;
log("counted", counted);

// A primitive right side is a TypeError, even after the function compiled.
for (const v of [1, "s", null, undefined, Symbol("q"), 1n]) {
  try { hot(() => A.hasF(a), 200); A.hasF(v); log("no throw", typeof v); } catch (e) { log("prim", typeof v, e.constructor.name); }
}

// Each evaluation of a class expression has its own names.
function make() { return class { #p; static has(o) { return #p in o; } }; }
const K1 = make(), K2 = make();
const k1 = new K1(), k2 = new K2();
log("repeated", hot(() => K1.has(k1)), hot(() => K1.has(k2)), hot(() => K2.has(k2)), hot(() => K2.has(k1)));

// An object that gains the field through a return override.
class Base { constructor(o) { return o; } }
class Stamp extends Base { #mark = 1; static is(o) { return #mark in o; } }
const target = {};
log("before stamp", hot(() => Stamp.is(target)));
new Stamp(target);
log("after stamp", Stamp.is(target), hot(() => Stamp.is(target)));
console.log(out.join("\n"));
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
